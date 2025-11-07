use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::exports::types::primitive::bounded::BoundedSize;
use edgli::hopr_lib::{Address, RoutingOptions};
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::command::{self, Command, Response, RunMode};
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::Destination;
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
}

pub struct Core {
    // config data
    config: Config,
    hopr_params: HoprParams,

    // cancellation tokens
    cancel_balances: CancellationToken,
    cancel_channel_funding: CancellationToken,
    cancel_connecting: CancellationToken,
    cancel_for_shutdown: CancellationToken,

    // user provided data
    target_destination: Option<Destination>,

    // runtime data
    phase: Phase,
    balances: Option<balance::Balances>,
    funded_channels: Vec<Address>,
    funding_tool: balance::FundingTool,
    hopr: Option<Arc<Hopr>>,
    ticket_value: Option<Balance<WxHOPR>>,
    ongoing_disconnections: Vec<connection::down::Down>,
    last_connection_errors: HashMap<Address, String>,
}

#[derive(Debug, Clone)]
enum Phase {
    Initial,
    CreatingSafe { presafe: Option<balance::PreSafe> },
    Starting,
    HoprSyncing,
    HoprRunning,
    HoprChannelsFunded,
    Connecting(connection::up::Up),
    Connected(connection::up::Up),
    ShuttingDown,
}

impl Core {
    pub async fn init(config_path: &Path, hopr_params: HoprParams) -> Result<Core, Error> {
        let config = config::read(config_path).await?;
        wg_tooling::available().await?;
        Ok(Core {
            // config data
            config,
            hopr_params,

            // cancellation tokens
            cancel_balances: CancellationToken::new(),
            cancel_channel_funding: CancellationToken::new(),
            cancel_connecting: CancellationToken::new(),
            cancel_for_shutdown: CancellationToken::new(),

            // user provided data
            target_destination: None,

            // runtime data
            phase: Phase::Initial,
            balances: None,
            funded_channels: Vec::new(),
            funding_tool: balance::FundingTool::NotStarted,
            hopr: None,
            ticket_value: None,
            ongoing_disconnections: Vec::new(),
            last_connection_errors: HashMap::new(),
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
                self.cancel_channel_funding.cancel();
                self.cancel_connecting.cancel();
                self.cancel_for_shutdown.cancel();
                if let Some(hopr) = self.hopr.clone() {
                    tracing::debug!("shutting down hopr");
                    hopr.shutdown().await;
                }
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
                    Phase::HoprRunning | Phase::HoprChannelsFunded | Phase::Connecting(_) | Phase::Connected(_) => {
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
                                let node_address = match self.hopr_params.calc_keys().await {
                                    Ok(keys) => keys.chain_key.public().to_address().to_string(),
                                    Err(err) => {
                                        tracing::warn!(%err, "failed to calculate node address");
                                        "unknown".to_string()
                                    }
                                };
                                let (node_xdai, node_wxhopr) = match presafe {
                                    Some(presafe) => (presafe.node_xdai, presafe.node_wxhopr),
                                    None => (Balance::default(), Balance::default()),
                                };
                                RunMode::PreparingSafe {
                                    node_address,
                                    node_xdai,
                                    node_wxhopr,
                                    funding_tool: self.funding_tool.clone(),
                                }
                            }
                            Phase::Starting => RunMode::ValueingTicket,
                            Phase::HoprRunning | Phase::HoprSyncing => {
                                if let Some(hopr) = &self.hopr {
                                    RunMode::Warmup {
                                        hopr_state: hopr.status().to_string(),
                                    }
                                } else {
                                    RunMode::Warmup {
                                        hopr_state: "unknown".to_string(),
                                    }
                                }
                            }
                            Phase::HoprChannelsFunded | Phase::Connecting(_) | Phase::Connected(_) => {
                                let funding =
                                    if let (Some(balances), Some(ticket_value)) = (&self.balances, self.ticket_value) {
                                        balances
                                            .to_funding_issues(self.config.channel_targets().len(), ticket_value)
                                            .into()
                                    } else {
                                        command::FundingState::Unknown
                                    };
                                if let Some(hopr) = &self.hopr {
                                    RunMode::Running {
                                        hopr_state: hopr.status().to_string(),
                                        funding,
                                    }
                                } else {
                                    RunMode::Running {
                                        hopr_state: "unknown".to_string(),
                                        funding,
                                    }
                                }
                            }
                            Phase::ShuttingDown => RunMode::Shutdown,
                        };

                        let destinations = self
                            .config
                            .destinations
                            .values()
                            .map(|v| {
                                let destination: command::Destination = v.into();
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
                                let last_connection_error = self.last_connection_errors.get(&v.address).cloned();
                                command::DestinationState {
                                    destination,
                                    connection_state,
                                    last_connection_error,
                                }
                            })
                            .collect();
                        let res = Response::status(command::StatusResponse::new(runmode, destinations));
                        let _ = resp.send(res);
                    }

                    Command::Connect(address) => match self.config.destinations.clone().get(&address) {
                        Some(dest) => {
                            self.target_destination = Some(dest.clone());
                            let _ = resp.send(Response::connect(command::ConnectResponse::new(dest.into())));
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
                                    (&conn.destination).into(),
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
                            let issues: Vec<balance::FundingIssue> =
                                balances.to_funding_issues(self.config.channel_targets().len(), ticket_value);

                            let res = command::BalanceResponse::new(
                                format!("{} xDai", balances.node_xdai),
                                format!("{} wxHOPR", balances.safe_wxhopr),
                                format!("{} wxHOPR", balances.channels_out_wxhopr),
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
                self.phase = Phase::HoprRunning;
                tracing::debug!(
                    channel_targets = ?self.config.channel_targets(),
                    "hopr is running - ensuring channel funding"
                );
                for c in self.config.channel_targets() {
                    self.spawn_channel_funding(c, results_sender, Duration::ZERO);
                }
                // only for testing purposes
                if self.config.channel_targets().is_empty() && self.hopr_params.allow_insecure() {
                    tracing::warn!(
                        "no channel targets configured and insecure mode enabled - operating without channels"
                    );
                    self.phase = Phase::HoprChannelsFunded;
                    self.act_on_target(results_sender);
                }
            }

            Results::FundChannel { address, res } => match res {
                Ok(()) => {
                    tracing::info!(%address, "channel funded");
                    self.funded_channels.push(address);
                    if self.funded_channels.len() == self.config.channel_targets().len() {
                        self.phase = Phase::HoprChannelsFunded;
                        tracing::info!("all channels funded - hopr is ready");
                        self.act_on_target(results_sender);
                    }
                }
                Err(err) => {
                    tracing::error!(%err, %address, "failed to ensure channel funding - retrying in 1 minute");
                    self.spawn_channel_funding(address, results_sender, Duration::from_secs(60));
                }
            },

            Results::ConnectionEvent { evt } => {
                tracing::debug!(%evt, "handling connection runner event");
                match self.phase.clone() {
                    Phase::Connecting(mut conn) => {
                        conn.connect_evt(evt);
                    }
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
                    self.last_connection_errors.remove(&conn.destination.address);
                    log_output::print_session_established(conn.destination.pretty_print_path().as_str());
                }
                (Ok(_), phase) => {
                    tracing::info!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(%conn, %err, "connection failed");
                    self.last_connection_errors
                        .insert(conn.destination.address, err.to_string());
                    if let Some(dest) = self.target_destination.clone()
                        && dest == conn.destination
                    {
                        tracing::info!(%dest, "removing target destination due to connection error");
                        self.target_destination = None;
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
    fn spawn_channel_funding(&self, address: Address, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        tracing::debug!(ticket_value = ?self.ticket_value, hopr_present  = self.hopr.is_some(), "checking channel funding");
        if let (Some(hopr), Some(ticket_value)) = (self.hopr.clone(), self.ticket_value) {
            let cancel = self.cancel_channel_funding.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::fund_channel(hopr, address, ticket_value, results_sender).await;
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
            (Some(dest), Phase::HoprChannelsFunded) => {
                tracing::info!(destination = %dest, "establishing connection to new destination");
                if matches!(dest.routing, RoutingOptions::Hops(n) if <BoundedSize<3> as Into<u8>>::into(n) == 0) {
                    if self.hopr_params.allow_insecure() {
                        tracing::warn!("connecting to destination with insecure 0 hops route");
                        self.spawn_connection_runner(dest.clone(), results_sender);
                    } else {
                        tracing::warn!(%dest, route = ?dest.routing, "refusing to connect to via insecure route to target destination");
                    }
                } else {
                    self.spawn_connection_runner(dest.clone(), results_sender);
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
        self.phase = Phase::HoprChannelsFunded;
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
        self.phase = Phase::HoprChannelsFunded;
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
        self.cancel_channel_funding.cancel();
        self.cancel_channel_funding = CancellationToken::new();
        self.phase = Phase::HoprRunning;
        self.funded_channels.clear();
        self.last_connection_errors.clear();
        for c in self.config.channel_targets() {
            self.spawn_channel_funding(c, results_sender, Duration::ZERO);
        }
    }
}
