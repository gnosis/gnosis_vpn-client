use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::command::{self, Command, Response, RunMode};
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::Destination;
use crate::connection::destination_health::{self, DestinationHealth};
use crate::external_event::Event as ExternalEvent;
use crate::hopr::{Hopr, HoprError, config as hopr_config, identity};
use crate::hopr_params::HoprParams;
use crate::{balance, log_output, wg_tooling};

pub mod runner;

use runner::Results;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WgTooling(#[from] wg_tooling::Error),
    #[error("HOPR error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Hopr config error: {0}")]
    HoprConfig(#[from] hopr_config::Error),
    #[error("Hopr identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Balance error: {0}")]
    Balance(#[from] balance::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    HoprParams(#[from] crate::hopr_params::Error),
}

pub struct Core {
    // config data
    config: Config,
    hopr_params: HoprParams,
    node_address: Address,

    // cancellation tokens
    cancel_balances: CancellationToken,
    cancel_channel_tasks: CancellationToken,
    cancel_connecting: CancellationToken,
    cancel_for_shutdown: CancellationToken,

    // user provided data
    target_destination: Option<Destination>,

    // runtime data
    phase: Phase,
    balances: Option<balance::Balances>,
    funding_tool: balance::FundingTool,
    hopr: Option<Arc<Hopr>>,
    ticket_value: Option<Balance<WxHOPR>>,
    destination_health: HashMap<Address, DestinationHealth>,
    ongoing_disconnections: Vec<connection::down::Down>,
}

#[derive(Debug, Clone)]
enum Phase {
    Initial,
    CreatingSafe { presafe: Option<balance::PreSafe> },
    Starting,
    HoprSyncing,
    HoprRunning,
    Connecting(connection::up::Up),
    Connected(connection::up::Up),
    ShuttingDown,
}

impl Core {
    pub async fn init(config_path: &Path, hopr_params: HoprParams) -> Result<Core, Error> {
        let config = config::read(config_path).await?;
        wg_tooling::available().await?;
        wg_tooling::executable().await?;
        let keys = hopr_params.generate_id_if_absent().await?;
        let node_address = keys.chain_key.public().to_address();
        Ok(Core {
            // config data
            config,
            hopr_params,
            node_address,

            // cancellation tokens
            cancel_balances: CancellationToken::new(),
            cancel_channel_tasks: CancellationToken::new(),
            cancel_connecting: CancellationToken::new(),
            cancel_for_shutdown: CancellationToken::new(),

            // user provided data
            target_destination: None,

            // runtime data
            phase: Phase::Initial,
            balances: None,
            funding_tool: balance::FundingTool::NotStarted,
            hopr: None,
            ticket_value: None,
            ongoing_disconnections: Vec::new(),
            destination_health: HashMap::new(),
        })
    }

    pub async fn start(mut self, event_receiver: &mut mpsc::Receiver<ExternalEvent>) {
        let (results_sender, mut results_receiver) = mpsc::channel(32);
        self.initial_runner(&results_sender);
        loop {
            tokio::select! {
                // React to an incoming outside event
                Some(event) = event_receiver.recv() => {
                    if self.on_event(event, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
                }
                // React to internal results from longer lasting runner computations
                Some(results) = results_receiver.recv() => {
                    self.on_results(results, &results_sender).await;
                }
                else => {
                    tracing::warn!("event receiver closed");
                    break;
                }
            }
        }
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    async fn on_event(&mut self, event: ExternalEvent, results_sender: &mpsc::Sender<Results>) -> bool {
        tracing::debug!(phase = ?self.phase, "on outside event");
        match event {
            ExternalEvent::Shutdown { resp } => {
                tracing::debug!("shutting down core");
                self.phase = Phase::ShuttingDown;
                self.cancel_balances.cancel();
                self.cancel_channel_tasks.cancel();
                self.cancel_connecting.cancel();
                self.cancel_for_shutdown.cancel();
                let shutdown_tracker = TaskTracker::new();
                shutdown_tracker.spawn(async {
                    // ensure wg is disconnected, ignore errors
                    let _ = wg_tooling::down().await;
                });
                if let Some(hopr) = self.hopr.clone() {
                    shutdown_tracker.spawn(async move {
                        tracing::debug!("shutting down hopr");
                        hopr.shutdown().await;
                    });
                }
                shutdown_tracker.close();
                shutdown_tracker.wait().await;
                if resp.send(()).is_err() {
                    tracing::warn!("shutdown receiver dropped");
                }
                false
            }

            ExternalEvent::ConfigReload { path } => {
                match self.phase {
                    Phase::ShuttingDown => {
                        tracing::warn!("ignoring configuration reload - shutting down");
                    }
                    Phase::Initial | Phase::CreatingSafe { .. } | Phase::Starting | Phase::HoprSyncing => {
                        let config = match config::read(&path).await {
                            Ok(cfg) => cfg,
                            Err(err) => {
                                tracing::warn!(%err, "failed to read configuration - keeping existing configuration");
                                return true;
                            }
                        };
                        self.config = config;
                    }
                    Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                        let config = match config::read(&path).await {
                            Ok(cfg) => cfg,
                            Err(err) => {
                                tracing::warn!(%err, "failed to read configuration - keeping existing configuration");
                                return true;
                            }
                        };
                        self.config = config;
                        self.reset_to_hopr_running(results_sender);
                    }
                }
                true
            }

            ExternalEvent::Command { cmd, resp } => {
                tracing::debug!(%cmd, "incoming command");
                match cmd {
                    Command::Status => {
                        let runmode = match self.phase.clone() {
                            Phase::Initial => RunMode::Init,
                            Phase::CreatingSafe { presafe } => {
                                RunMode::preparing_safe(self.node_address, presafe, self.funding_tool.clone())
                            }
                            Phase::Starting => RunMode::ValueingTicket,
                            Phase::HoprSyncing => RunMode::warmup(self.hopr.as_ref().map(|h| h.status())),
                            Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                                let funding =
                                    if let (Some(balances), Some(ticket_value)) = (&self.balances, self.ticket_value) {
                                        let min_channel_count = destination_health::count_distinct_channels(
                                            &self.destination_health.values().collect::<Vec<_>>(),
                                        );
                                        balances.to_funding_issues(min_channel_count, ticket_value).into()
                                    } else {
                                        Default::default()
                                    };
                                RunMode::running(funding, self.hopr.as_ref().map(|h| h.status()))
                            }
                            Phase::ShuttingDown => RunMode::Shutdown,
                        };

                        let mut vals = self.config.destinations.values().collect::<Vec<&Destination>>();
                        vals.sort_by(|a, b| a.address.cmp(&b.address));
                        let destinations = vals
                            .into_iter()
                            .map(|v| {
                                let destination = v.clone();
                                let connection_state = match &self.phase {
                                    Phase::Connecting(conn) if &conn.destination == v => {
                                        command::ConnectionState::Connecting(conn.phase.0, conn.phase.1.clone())
                                    }
                                    Phase::Connected(conn) if &conn.destination == v => {
                                        command::ConnectionState::Connected(conn.phase.0)
                                    }
                                    _ => {
                                        if let Some(disconn) =
                                            self.ongoing_disconnections.iter().find(|d| &d.destination == v)
                                        {
                                            command::ConnectionState::Disconnecting(
                                                disconn.phase.0,
                                                disconn.phase.1.clone(),
                                            )
                                        } else {
                                            command::ConnectionState::None
                                        }
                                    }
                                };
                                command::DestinationState {
                                    destination,
                                    connection_state,
                                    health: self.destination_health.get(&v.address).cloned(),
                                }
                            })
                            .collect();
                        let res = Response::status(command::StatusResponse::new(runmode, destinations));
                        let _ = resp.send(res);
                    }

                    Command::Connect(address) => match self.config.destinations.clone().get(&address) {
                        Some(dest) => {
                            self.target_destination = Some(dest.clone());
                            let _ = resp.send(Response::connect(command::ConnectResponse::new(dest.clone())));
                            self.act_on_target(results_sender);
                        }
                        None => {
                            tracing::info!(address = %address, "cannot connect to destination - address not configured");
                            let _ = resp.send(Response::connect(command::ConnectResponse::address_not_found()));
                        }
                    },

                    Command::Disconnect => {
                        self.target_destination = None;
                        match self.phase.clone() {
                            Phase::Connected(conn) | Phase::Connecting(conn) => {
                                tracing::info!(current = %conn.destination, "disconnecting");
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::new(
                                    conn.destination.clone(),
                                )));
                            }
                            _ => {
                                tracing::debug!("no active connection to disconnect");
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::not_connected()));
                            }
                        }
                        self.act_on_target(results_sender);
                    }

                    Command::Balance => {
                        if let (Some(hopr), Some(balances), Some(ticket_value)) =
                            (self.hopr.clone(), self.balances.clone(), self.ticket_value)
                        {
                            let info = hopr.info();
                            let min_channel_count = destination_health::count_distinct_channels(
                                &self.destination_health.values().collect::<Vec<_>>(),
                            );
                            let issues: Vec<balance::FundingIssue> =
                                balances.to_funding_issues(min_channel_count, ticket_value);

                            let res = command::BalanceResponse::new(
                                balances.node_xdai,
                                balances.safe_wxhopr,
                                balances.channels_out_wxhopr,
                                issues,
                                info,
                            );
                            let _ = resp.send(Response::Balance(Some(res)));
                        } else {
                            let _ = resp.send(Response::Balance(None));
                        }
                    }

                    Command::Ping => {
                        let _ = resp.send(Response::Pong);
                    }

                    Command::RefreshNode => {
                        // immediately request balances and cancel existing balance loop
                        self.cancel_balances.cancel();
                        self.cancel_balances = CancellationToken::new();
                        self.spawn_balances_runner(results_sender, Duration::ZERO);
                        let _ = resp.send(Response::Empty);
                    }

                    Command::FundingTool(secret) => {
                        if matches!(self.phase, Phase::CreatingSafe { .. }) {
                            self.funding_tool = balance::FundingTool::InProgress;
                            self.spawn_funding_runner(secret, results_sender);
                            let _ = resp.send(Response::Empty);
                        } else {
                            tracing::warn!("cannot start funding tool - safe already deployed");
                            let _ = resp.send(Response::Empty);
                        }
                    }

                    Command::Metrics => {
                        let metrics = match edgli::hopr_lib::Hopr::collect_hopr_metrics() {
                            Ok(m) => m,
                            Err(err) => {
                                tracing::error!(%err, "failed to collect hopr metrics");
                                String::new()
                            }
                        };
                        let _ = resp.send(Response::Metrics(metrics));
                    }
                }
                true
            }
        }
    }

    #[tracing::instrument(skip(self, results_sender, results), level = "debug", ret)]
    async fn on_results(&mut self, results: Results, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(phase = ?self.phase, %results, "on runner results");
        match results {
            Results::TicketStats { res } => match res {
                Ok(stats) => match stats.ticket_value() {
                    Ok(tv) => {
                        tracing::info!(%stats, %tv, "determined ticket value from stats");
                        self.ticket_value = Some(tv);
                        self.spawn_hopr_runner(results_sender, Duration::ZERO);
                    }
                    Err(err) => {
                        tracing::error!(%stats, %err, "failed to determine ticket value from stats - retrying");
                        self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
                    }
                },
                Err(err) => {
                    tracing::error!(%err, "failed to fetch ticket stats - retrying");
                    self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::PreSafe { res } => match res {
                Ok(presafe) => {
                    tracing::info!(%presafe, "on presafe balance");
                    if presafe.node_xdai.is_zero() || presafe.node_wxhopr.is_zero() {
                        tracing::warn!("insufficient funds to start safe deployment - waiting");
                        self.spawn_presafe_runner(results_sender, Duration::from_secs(10));
                    } else {
                        self.spawn_safe_deployment_runner(&presafe, results_sender);
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "failed to fetch presafe balance - retrying");
                    self.spawn_presafe_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::SafeDeployment { res } => match res {
                Ok(deployment) => {
                    self.spawn_store_safe(deployment.into(), results_sender);
                }
                Err(err) => {
                    tracing::error!(%err, "error deploying safe module - rechecking balance");
                    self.spawn_presafe_runner(results_sender, Duration::from_secs(5));
                }
            },

            Results::SafePersisted => {
                tracing::info!("safe module persisted - starting hopr runner");
                self.phase = Phase::Starting;
                self.spawn_hopr_runner(results_sender, Duration::ZERO);
            }

            Results::Hopr { res } => match res {
                Ok(hopr) => {
                    tracing::info!("hopr runner started successfully");
                    self.phase = Phase::HoprSyncing;
                    self.hopr = Some(Arc::new(hopr));
                    self.spawn_balances_runner(results_sender, Duration::ZERO);
                    self.spawn_wait_for_running(results_sender, Duration::from_secs(1));
                }
                Err(err) => {
                    tracing::error!(%err, "hopr runner failed to start - trying again in 10 seconds");
                    self.spawn_hopr_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::FundingTool { res } => match res {
                Ok(success) => {
                    self.funding_tool = if success {
                        balance::FundingTool::CompletedSuccess
                    } else {
                        balance::FundingTool::CompletedError
                    };
                }
                Err(err) => {
                    tracing::error!(%err, "funding runner exited with error");
                    self.funding_tool = balance::FundingTool::CompletedError;
                }
            },

            Results::Balances { res } => match res {
                Ok(balances) => {
                    tracing::info!(%balances, "received balances from hopr");
                    self.balances = Some(balances);
                    self.spawn_balances_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::error!(%err, "failed to fetch balances from hopr");
                    self.spawn_balances_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::HoprRunning => {
                self.on_hopr_running(results_sender);
            }

            Results::ConnectedPeers { res } => match res {
                Ok(peers) => {
                    tracing::info!(num_peers = %peers.len(), "fetched connected peers");
                    let all_peers = HashSet::from_iter(peers.iter().cloned());
                    for (target, health) in self.destination_health.clone() {
                        let updated_health = health.peers(&all_peers);
                        if let Some(addr) = updated_health.needs_channel_funding() {
                            self.spawn_channel_funding(addr, target, results_sender, Duration::ZERO);
                        }
                        self.destination_health.insert(target, updated_health);
                    }
                    self.spawn_connected_peers(results_sender, Duration::from_secs(90));
                }
                Err(err) => {
                    tracing::error!(%err, "failed to fetch connected peers");
                    self.spawn_connected_peers(results_sender, Duration::from_secs(10));
                }
            },

            Results::FundChannel {
                address,
                res,
                target_dest,
            } => match res {
                Ok(()) => {
                    tracing::info!(%address, "channel funded");
                    self.update_health(target_dest, |h| h.channel_funded(address));
                }
                Err(err) => {
                    tracing::error!(%err, %address, "failed to ensure channel funding - retrying in 1 minute if needed");
                    if self.update_health(target_dest, |h| h.with_error(err.to_string())) {
                        self.spawn_channel_funding(address, target_dest, results_sender, Duration::from_secs(60));
                    }
                }
            },

            Results::ConnectionEvent { evt } => {
                tracing::debug!(%evt, "handling connection runner event");
                match self.phase.clone() {
                    Phase::Connecting(mut conn) => match evt {
                        connection::up::runner::Event::Progress(e) => {
                            conn.connect_progress(e);
                            self.phase = Phase::Connecting(conn);
                        }
                        connection::up::runner::Event::Setback(e) => {
                            self.update_health(conn.destination.address, |h| h.with_error(e.to_string()));
                        }
                    },
                    phase => {
                        tracing::warn!(?phase, %evt, "received connection event in unexpected phase");
                    }
                }
            }

            Results::DisconnectionEvent { wg_public_key, evt } => {
                tracing::debug!(%wg_public_key, %evt, "handling disconnection runner event");
                if let Some(conn) = self
                    .ongoing_disconnections
                    .iter_mut()
                    .find(|c| c.wg_public_key == wg_public_key)
                {
                    conn.disconnect_evt(evt);
                } else {
                    tracing::warn!(?self.phase, %evt, "received disconnection event for unknown connection");
                }
                // potentially reconnect early after wg disconnected
                // this might only happen when reconnecting to a different destination after a
                // connection established successfully
                if matches!(evt, connection::down::runner::Event::OpenBridge) {
                    self.act_on_target(results_sender);
                }
            }

            Results::ConnectionResult { res } => match (res, self.phase.clone()) {
                (Ok(_), Phase::Connecting(mut conn)) => {
                    tracing::info!(%conn, "connection established successfully");
                    conn.connected();
                    self.phase = Phase::Connected(conn.clone());
                    self.update_health(conn.destination.address, |h| h.no_error());
                    log_output::print_session_established(conn.destination.pretty_print_path().as_str());
                }
                (Ok(_), phase) => {
                    tracing::info!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(%conn, %err, "connection failed");
                    self.update_health(conn.destination.address, |h| h.with_error(err.to_string()));
                    if let Some(dest) = self.target_destination.clone()
                        && dest == conn.destination
                    {
                        tracing::info!(%dest, "removing target destination due to connection error");
                        self.target_destination = None;
                        self.act_on_target(results_sender);
                    }
                }
                (Err(err), phase) => {
                    tracing::warn!(?phase, %err, "connection failed in unexpecting state");
                }
            },

            Results::DisconnectionResult { wg_public_key, res } => {
                match res {
                    Ok(_) => {
                        tracing::info!(%wg_public_key, "disconnected successful");
                    }
                    Err(err) => {
                        tracing::error!(%wg_public_key, %err, "disconnection failed");
                    }
                }
                self.ongoing_disconnections.retain(|c| c.wg_public_key != wg_public_key);
                self.act_on_target(results_sender);
            }
        }
    }

    fn initial_runner(&mut self, results_sender: &mpsc::Sender<Results>) {
        if hopr_config::has_safe() {
            self.phase = Phase::Starting;
        } else {
            self.phase = Phase::CreatingSafe { presafe: None };
            self.spawn_presafe_runner(results_sender, Duration::ZERO);
        }
        self.spawn_ticket_stats_runner(results_sender, Duration::ZERO);
    }

    fn spawn_ticket_stats_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_for_shutdown.clone();
        let hopr_params = self.hopr_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::ticket_stats(hopr_params, results_sender).await;
                })
                .await
        });
    }

    fn spawn_presafe_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_for_shutdown.clone();
        let hopr_params = self.hopr_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::presafe(hopr_params, results_sender).await
                })
                .await
        });
    }

    fn spawn_funding_runner(&self, secret: String, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown.clone();
        let hopr_params = self.hopr_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move { runner::funding_tool(hopr_params, secret, results_sender).await })
                .await;
        });
    }

    fn spawn_safe_deployment_runner(&self, presafe: &balance::PreSafe, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown.clone();
        let hopr_params = self.hopr_params.clone();
        let presafe = presafe.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::safe_deployment(hopr_params, presafe, results_sender).await;
                })
                .await
        });
    }

    fn spawn_store_safe(&mut self, safe_module: hopr_config::SafeModule, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::persist_safe(safe_module, results_sender).await;
                })
                .await
        });
    }

    fn spawn_hopr_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        // check if we are ready: safe available(Phase::Starting) and ticket value
        if let (Phase::Starting, Some(ticket_value)) = (self.phase.clone(), self.ticket_value) {
            let cancel = self.cancel_for_shutdown.clone();
            let hopr_params = self.hopr_params.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::hopr(hopr_params, ticket_value, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_balances_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_balances.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::balances(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_wait_for_running(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_for_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::wait_for_running(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    fn spawn_channel_funding(
        &self,
        address: Address,
        target_dest: Address,
        results_sender: &mpsc::Sender<Results>,
        delay: Duration,
    ) {
        tracing::debug!(ticket_value = ?self.ticket_value, hopr_present  = self.hopr.is_some(), "checking channel funding");
        if let (Some(hopr), Some(ticket_value)) = (self.hopr.clone(), self.ticket_value) {
            let cancel = self.cancel_channel_tasks.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::fund_channel(hopr, address, ticket_value, target_dest, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connected_peers(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_channel_tasks.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::connected_peers(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connection_runner(&mut self, destination: Destination, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connecting.clone();
            let conn = connection::up::Up::new(destination.clone());
            let config_connection = self.config.connection.clone();
            let config_wireguard = self.config.wireguard.clone();
            let hopr = hopr.clone();
            let runner = connection::up::runner::Runner::new(conn.clone(), config_connection, config_wireguard, hopr);
            let results_sender = results_sender.clone();
            self.phase = Phase::Connecting(conn);
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner.start(results_sender).await;
                    })
                    .await;
            });
        }
    }

    fn spawn_disconnection_runner(&mut self, disconn: &connection::down::Down, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_for_shutdown.clone();
            let config_connection = self.config.connection.clone();
            let hopr = hopr.clone();
            let runner = connection::down::runner::Runner::new(disconn.clone(), hopr, config_connection);
            let results_sender = results_sender.clone();
            self.ongoing_disconnections.push(disconn.clone());
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner.start(results_sender).await;
                    })
                    .await;
            });
        }
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    fn act_on_target(&mut self, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(target = ?self.target_destination, phase = ?self.phase, "acting on target destination");
        match (self.target_destination.clone(), self.phase.clone()) {
            // Connecting from ready
            (Some(dest), Phase::HoprRunning) => {
                // Checking health
                if let Some(health) = self.destination_health.get(&dest.address) {
                    if health.is_ready_to_connect() {
                        tracing::info!(destination = %dest, "establishing connection to new destination");
                        self.spawn_connection_runner(dest.clone(), results_sender);
                    } else if health.is_unrecoverable() {
                        tracing::error!(?health, destination = %dest, "refusing connection because of destination health");
                    } else {
                        tracing::warn!(?health, destination = %dest, "waiting for better destination health before connecting");
                    }
                } else {
                    tracing::warn!(destination = %dest, "refusing connection: destination has no health tracker");
                }
            }
            // Connecting to different destination while already connected
            (Some(dest), Phase::Connected(conn)) if dest != conn.destination => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while already connected");
                self.disconnect_from_connected(&conn, results_sender);
            }
            // Connecting to different destination while already connecting
            (Some(dest), Phase::Connecting(conn)) if dest != conn.destination => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while already connecting");
                self.disconnect_from_connecting(&conn, results_sender);
            }
            // Disconnecting from established connection
            (None, Phase::Connected(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from destination");
                self.disconnect_from_connected(&conn, results_sender);
            }
            // Disconnecting while establishing connection
            (None, Phase::Connecting(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from ongoing connection attempt");
                self.disconnect_from_connecting(&conn, results_sender);
            }
            // No action needed
            _ => {}
        }
    }

    fn disconnect_from_connected(&mut self, conn: &connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        match conn.try_into() {
            Ok(disconn) => self.spawn_disconnection_runner(&disconn, results_sender),
            Err(err) => {
                tracing::error!(%err, "failed to create disconnection runner from connection");
            }
        }
    }

    fn disconnect_from_connecting(&mut self, conn: &connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        self.cancel_connecting.cancel();
        self.cancel_connecting = CancellationToken::new();
        self.phase = Phase::HoprRunning;
        if let Ok(disconn) = conn.try_into() {
            self.spawn_disconnection_runner(&disconn, results_sender);
        } else {
            // connection did not even generate a wg pub key - so we can immediately try to connect again
            self.act_on_target(results_sender);
        }
    }

    fn reset_to_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        match self.phase.clone() {
            Phase::Connected(conn) => {
                self.disconnect_from_connected(&conn, results_sender);
            }
            Phase::Connecting(conn) => {
                self.disconnect_from_connecting(&conn, results_sender);
            }
            _ => (),
        }
        self.cancel_channel_tasks.cancel();
        self.cancel_channel_tasks = CancellationToken::new();
        self.destination_health.clear();
        self.on_hopr_running(results_sender);
    }

    fn on_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        for (address, dest) in self.config.destinations.clone() {
            self.destination_health.insert(
                address,
                DestinationHealth::from_destination(&dest, self.hopr_params.allow_insecure()),
            );
        }
        if destination_health::needs_peers(&self.destination_health.values().collect::<Vec<_>>()) {
            self.spawn_connected_peers(results_sender, Duration::ZERO);
        }
        self.act_on_target(results_sender);
    }

    fn update_health<F>(&mut self, address: Address, cb: F) -> bool
    where
        F: Fn(&DestinationHealth) -> DestinationHealth,
    {
        if let Some(health) = self.destination_health.get(&address) {
            self.destination_health.insert(address, cb(health));
            true
        } else {
            tracing::warn!(?address, "connection destination has no health tracker");
            false
        }
    }
}
