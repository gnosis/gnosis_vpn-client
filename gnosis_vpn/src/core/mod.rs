use edgli::hopr_lib::Address;
// use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, destination::Destination};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, config as hopr_config, identity};
use gnosis_vpn_lib::{balance, wg_tooling};

use crate::event::Event;
use crate::hopr_params::HoprParams;

mod conn;
mod connection_runner;
mod runner;

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
    // configuration data
    config: Config,
    // depending on safe creation state
    // run_mode: RunMode,
    // enable cancellation of tasks
    cancel_for_shutdown_token: CancellationToken,
    // enable balance loop cancellation separately
    cancel_balances_token: CancellationToken,

    // connection to exit node
    connection: Option<connection::Connection>,
    session_connected: bool,
    target_destination: Option<Destination>,

    // provided hopr params
    hopr_params: HoprParams,

    // results from node
    balances: Option<balance::Balances>,

    // results from onboarding
    funding_tool: balance::FundingTool,

    // supposedly working channels (funding was ok)
    funded_channels: Vec<Address>,

    // results from ticket stats
    ticket_value: Option<Balance<WxHOPR>>,
    phase: Phase,
    hopr: Option<Arc<Hopr>>,
}

#[derive(Debug, Clone)]
enum Phase {
    Initial,
    CreatingSafe,
    Starting,
    HoprSyncing,
    HoprRunning,
    HoprChannelsFunded,
    // Connecting(ConnUp),
    // ConnectingFailed(String),
    // Disconnecting(ConnDown),
    ShuttingDown,
}

#[derive(Clone)]
enum RunMode {
    /*
    Initializing,
    PreSafe {
        node_address: Address,
        onboarding: Box<Onboarding>,
    },
    ValueingTicket,
    Syncing {
        hopr: Arc<Hopr>,
        ticket_value: Balance<WxHOPR>,
        // thread loop around funding and general node
        node: Box<Node>,
        // thread loop around gathering metrics
        metrics: Box<Metrics>,
    },
    Full {
        hopr: Arc<Hopr>,
        #[allow(dead_code)]
        ticket_value: Balance<WxHOPR>,
        // thread loop around funding and general node
        #[allow(dead_code)]
        node: Box<Node>,
        // thread loop around channel funding
        #[allow(dead_code)]
        channel_funding: Box<ChannelFunding>,
    },
    */
}

enum Cancel {
    Node,
    Connection,
    Onboarding,
    ChannelFunding,
    Metrics,
    ValueingTicket,
}

impl Core {
    pub async fn init(config_path: &Path, hopr_params: HoprParams) -> Result<Core, Error> {
        let config = config::read(config_path).await?;
        wg_tooling::available().await?;
        Ok(Core {
            config,
            cancel_for_shutdown_token: CancellationToken::new(),
            cancel_balances_token: CancellationToken::new(),
            connection: None,
            // run_mode: RunMode::Initializing,
            session_connected: false,
            target_destination: None,
            balances: None,
            hopr_params,
            hopr: None,
            funding_tool: balance::FundingTool::NotStarted,
            funded_channels: Vec::new(),
            ticket_value: None,
            phase: Phase::Initial,
        })
    }

    pub async fn start(mut self, event_receiver: &mut mpsc::Receiver<Event>) {
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

    #[tracing::instrument(skip(self), level = "debug", ret)]
    async fn on_event(&mut self, event: Event, results_sender: &mpsc::Sender<Results>) -> bool {
        tracing::debug!(phase = ?self.phase, "on outside event");
        match event {
            Event::Shutdown { resp } => {
                tracing::debug!("shutting down core");
                self.phase = Phase::ShuttingDown;
                self.cancel_for_shutdown_token.cancel();
                self.cancel_balances_token.cancel();
                if let Some(hopr) = self.hopr.clone() {
                    tracing::debug!("shutting down hopr");
                    hopr.shutdown().await;
                }
                if resp.send(()).is_err() {
                    tracing::warn!("shutdown receiver dropped");
                }
                false
            }
            Event::Command { cmd, resp } => {
                tracing::debug!(%cmd, "incoming command");
                match cmd {
                    Command::Status => {
                        // let run_mode = match &self.run_mode {
                        //     RunMode::Initializing => command::RunMode::initializing(),
                        //     RunMode::PreSafe { node_address, .. } => {
                        //         let balance = self.presafe_balance.clone().unwrap_or_default();
                        //         let funding_tool = self.funding_tool.clone();
                        //         command::RunMode::preparing_safe(*node_address, balance, funding_tool)
                        //     }
                        //     RunMode::Syncing { hopr, .. } => {
                        //         let syncing = self.metrics.clone().map(|m| m.sync_percentage).unwrap_or_default();
                        //         command::RunMode::warmup(syncing, hopr.status().to_string())
                        //     }
                        //     RunMode::ValueingTicket { .. } => command::RunMode::valueing_ticket(),
                        //     RunMode::Full { hopr, .. } => {
                        //         let connection_state = match (
                        //             self.target_destination.clone(),
                        //             self.connection.clone().map(|c| c.destination()),
                        //             self.session_connected,
                        //         ) {
                        //             (Some(dest), _, true) => command::ConnectionState::connected(dest.clone().into()),
                        //             (Some(dest), _, false) => command::ConnectionState::connecting(dest.clone().into()),
                        //             (None, Some(conn_dest), _) => command::ConnectionState::disconnecting(conn_dest.into()),
                        //             (None, None, _) => command::ConnectionState::disconnected(),
                        //         };
                        //         let funding_state: command::FundingState =
                        //             if let (Some(balance), Some(ticket_stats)) = (&self.balance, &self.ticket_stats) {
                        //                 let ticket_value = ticket_stats.ticket_value()?;
                        //                 balance
                        //                     .to_funding_issues(self.config.channel_targets().len(), ticket_value)
                        //                     .into()
                        //             } else {
                        //                 command::FundingState::Unknown
                        //             };

                        //         command::RunMode::running(connection_state, funding_state, hopr.status().to_string())
                        //     }
                        // };

                        let available_destinations = self
                            .config
                            .destinations
                            .values()
                            .map(|v| {
                                let dest = v.clone();
                                dest.into()
                            })
                            .collect();
                        let network = self.hopr_params.network.clone();
                        let res = Response::status(command::StatusResponse::new(
                            command::RunMode::initializing(),
                            available_destinations,
                            network,
                        ));
                        let _ = resp.send(res);
                    }
                    Command::Connect(address) => match self.config.destinations.clone().get(&address) {
                        Some(dest) => {
                            self.target_destination = Some(dest.clone());
                            let _ = resp.send(Response::connect(command::ConnectResponse::new(dest.clone().into())));
                            self.act_on_target().await;
                        }
                        None => {
                            tracing::info!(address = %address, "cannot connect to destination - address not configured");
                            let _ = resp.send(Response::connect(command::ConnectResponse::address_not_found()));
                        }
                    },
                    Command::Disconnect => {
                        self.target_destination = None;
                        let dest = self.connection.as_ref().map(|c| c.destination());
                        match dest {
                            Some(d) => {
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::new(d.into())));
                            }
                            None => {
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::not_connected()));
                            }
                        }
                        self.act_on_target().await;
                    }

                    Command::Balance => {
                        self.spawn_balance_response(&resp);
                    }
                    Command::Ping => {
                        let _ = resp.send(Response::Pong);
                    }
                    Command::RefreshNode => {
                        unimplemented!();
                        /*
                        // immediately request balances and cancel existing balance loop
                        self.balances_cancel_for_shutdown_token.cancel();
                        self.balances_cancel_for_shutdown_token = CancellationToken::new();
                        if let Some(cmd_sender) = self.hopr_cmd_sender.clone() {
                            tracing::debug!("requesting balances from hopr");
                            let _ = cmd_sender.send(hopr_runner::Cmd::Balances).await;
                        }
                        let _ = resp.send(Response::Empty);
                        */
                    }
                    Command::FundingTool(secret) => {
                        if matches!(self.phase, Phase::CreatingSafe) {
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
            evt => {
                unimplemented!("unhandled event: {evt:?}");
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
                for c in self.config.channel_targets() {
                    self.spawn_channel_funding(c, results_sender, Duration::ZERO);
                }
            }
            Results::FundChannel { address, res } => match res {
                Ok(()) => {
                    tracing::info!(%address, "channel funded");
                    self.funded_channels.push(address);
                    if self.funded_channels.len() == self.config.channel_targets().len() {
                        self.phase = Phase::HoprChannelsFunded;
                        tracing::info!("all channels funded - hopr is ready");
                        self.act_on_target().await;
                    }
                }
                Err(err) => {
                    tracing::error!(%err, %address, "failed to ensure channel funding - retrying in 1 minute");
                    self.spawn_channel_funding(address, results_sender, Duration::from_secs(60));
                }
            },
        }
    }

    fn initial_runner(&mut self, results_sender: &mpsc::Sender<Results>) {
        if hopr_config::has_safe() {
            self.phase = Phase::Starting;
        } else {
            self.phase = Phase::CreatingSafe;
            self.spawn_presafe_runner(results_sender, Duration::ZERO);
        }
        self.spawn_ticket_stats_runner(results_sender, Duration::ZERO);
    }

    fn spawn_ticket_stats_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_for_shutdown_token.clone();
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
        let cancel = self.cancel_for_shutdown_token.clone();
        let hopr_params = self.hopr_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::presafe(hopr_params, results_sender.clone()).await
                })
                .await
        });
    }

    fn spawn_funding_runner(&self, secret: String, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown_token.clone();
        let hopr_params = self.hopr_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(
                    async move { runner::funding_tool(hopr_params, secret, results_sender.clone()).await },
                )
                .await;
        });
    }

    fn spawn_safe_deployment_runner(&self, presafe: &balance::PreSafe, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown_token.clone();
        let hopr_params = self.hopr_params.clone();
        let presafe = presafe.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::safe_deployment(hopr_params, presafe, results_sender.clone()).await;
                })
                .await
        });
    }

    fn spawn_store_safe(&mut self, safe_module: hopr_config::SafeModule, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_for_shutdown_token.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::persist_safe(safe_module, results_sender.clone()).await;
                })
                .await
        });
    }

    fn spawn_hopr_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        // check if we are ready: safe available(Phase::Starting) and ticket value
        if let (Phase::Starting, Some(ticket_value)) = (self.phase.clone(), self.ticket_value) {
            let cancel = self.cancel_for_shutdown_token.clone();
            let hopr_params = self.hopr_params.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::hopr(hopr_params, ticket_value, results_sender.clone()).await;
                    })
                    .await
            });
        }
    }

    fn spawn_balances_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_balances_token.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::balances(hopr, results_sender.clone()).await;
                    })
                    .await
            });
        }
    }

    fn spawn_wait_for_running(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_for_shutdown_token.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::wait_for_running(hopr, results_sender.clone()).await;
                    })
                    .await
            });
        }
    }

    fn spawn_channel_funding(&self, address: Address, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let (Some(hopr), Some(ticket_value)) = (self.hopr.clone(), self.ticket_value) {
            let cancel = self.cancel_for_shutdown_token.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::fund_channel(hopr, address, ticket_value, results_sender.clone()).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connection_runner(&self, destination: Destination) {
        if let Some(hopr) = self.hopr.clone() {
            let hopr = hopr.clone();
            let config_wireguard = self.config.wireguard.clone();
            let config_connection = self.config.connection.clone();
            tokio::spawn(async move {
                let runner = connection_runner::ConnectionRunner::new(
                    destination.clone(),
                    config_connection,
                    config_wireguard,
                    hopr,
                );
                let (sender, mut receiver) = mpsc::channel(32);
                tokio::spawn(async move {
                    runner.connect(&sender).await;
                });
                while let Some(evt) = receiver.recv().await {
                    tracing::debug!(?evt, "connection event");
                    match evt {
                        connection_runner::Evt::Error(id, err) => {
                            tracing::error!(%id, %err, "connection runner error");
                            break;
                        }
                        _ => {
                            // TODO handle other events
                        }
                    }
                }
            });
        }
    }

    fn spawn_balance_response(&self, resp: &oneshot::Sender<Response>) {
        unimplemented!();
        /*
                        if let (Some(cmd_sender), Some(balances), Some(Ok(ticket_value))) = (
                            self.hopr_cmd_sender.clone(),
                            self.balances.clone(),
                            self.ticket_stats.map(|ts| ts.ticket_value()),
                        ) {
                            let (tx, rx) = oneshot::channel();
                            let _ = cmd_sender.send(hopr_runner::Cmd::Info { rsp: tx }).await;
                            let info = match rx.await {
                                Ok(info) => info,
                                Err(err) => {
                                    tracing::error!(%err, "failed to get hopr info for balance command");
                                    let _ = resp.send(Response::Balance(None));
                                    return true;
                                }
                            };
                            let issues: Vec<balance::FundingIssue> =
                                balances.to_funding_issues(self.config.channel_targets().len(), ticket_value);

                            let res = command::BalanceResponse::new(
                                format!("{} xDai", balances.node_xdai),
                                format!("{} wxHOPR", balances.safe_wxhopr),
                                format!("{} wxHOPR", balances.channels_out_wxhopr),
                                issues,
                                command::Addresses {
                                    node: info.node_address,
                                    safe: info.safe_address,
                                },
                            );
                            let _ = resp.send(Response::Balance(Some(res)));
                        } else {
                            let _ = resp.send(Response::Balance(None));
                        }
        */
    }

    /*
        let runner = presafe_runner::PreSafeRunner::new(self.hopr_params.clone());
        let cancel = self.cancel_for_shutdown_token.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            time::sleep(delay).await;
            tracing::debug!("starting presafe balance runner");
            let res = cancel.run_until_cancelled(runner.start()).await;
            if let Some(res) = res {
                let _ = results_sender
                    .send(RunnerResults::PreSafe(res.map_err(runner_results::Error::PreSafe)))
                    .await;
            }
        });
    }
    */

    async fn act_on_target(&mut self) {
        match (self.phase.clone(), self.target_destination.clone()) {
            (Phase::HoprChannelsFunded, Some(dest)) => {
                tracing::info!(destination = %dest, "establishing new connection");
                self.spawn_connection_runner(dest.clone());
            }
            _ => {
                unimplemented!()
            }
        }
    }
}

/*
        match event {
            Event::ConfigReload { path }=> {
                tracing::debug!("reloading configuration");
                let config = match config::read(&path) {
                    Ok(cfg) => cfg,
                    Err(err) => {
                        tracing::warn!(%err, "failed to read configuration - keeping existing configuration");
                        continue;
                    }
                };
                if self.config == config {
                    tracing::debug!("configuration unchanged - no action taken");
                    continue;
                }
                self.config = config;
                self.cancel_for_shutdown_token.cancel();
                self.run_mode = RunMode::Initializing;
            }
            evt => {
                tracing::debug!(%evt, "handling event");
                // TODO implement event handling
                unimplemented!()
            }
        }
    }
    else => {
        tracing::warn!("event receiver closed");
    }
}
*/

/*
    fn determine_run_mode(&mut self) -> Result<RunMode, Error> {
        match self.run_mode.clone() {
            RunMode::Initializing => {
                if hopr_config::has_safe() {
                    tracing::debug!("safe found: init -> valueing ticket");
                    let keys = calc_keys(&self.hopr_params)?;
                    fetch_ticket_stats(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        keys.chain_key,
                        self.hopr_params.rpc_provider.clone(),
                        self.hopr_params.network.clone(),
                    );
                    Ok(RunMode::ValueingTicket)
                } else {
                    tracing::debug!("safe not found: init -> onboarding");
                    let keys = calc_keys(&self.hopr_params)?;
                    let node_address = keys.chain_key.public().to_address();
                    let onboarding = setup_onboarding(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        keys.chain_key.clone(),
                        &self.hopr_params,
                        node_address,
                    );
                    onboarding.run();
                    Ok(RunMode::PreSafe {
                        node_address,
                        onboarding: Box::new(onboarding),
                    })
                }
            }
            RunMode::PreSafe {
                node_address: _,
                onboarding: _,
            } => {
                if hopr_config::has_safe() {
                    tracing::debug!("safe found: onboarding -> valueing ticket");
                    _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
                        tracing::error!(%e, "failed to send cancel event to onboarding");
                    });
                    let keys = calc_keys(&self.hopr_params)?;
                    fetch_ticket_stats(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        keys.chain_key,
                        self.hopr_params.rpc_provider.clone(),
                        self.hopr_params.network.clone(),
                    );
                    Ok(RunMode::ValueingTicket)
                } else {
                    tracing::debug!("safe not found: onboarding");
                    Ok(self.run_mode.clone())
                }
            }
            RunMode::ValueingTicket => {
                if let Some(stats) = &self.ticket_stats {
                    tracing::debug!("ticket stats: valueing ticket -> syncing");
                    _ = self.cancel_channel.0.send(Cancel::ValueingTicket).map_err(|e| {
                        tracing::error!(%e, "failed to send cancel event to onboarding");
                    });
                    let ticket_value = stats.ticket_value()?;
                    let cfg = match self.hopr_params.config_mode.clone() {
                        // use user provided configuration path
                        hopr_params::ConfigFileMode::Manual(path) => hopr_config::from_path(path.as_ref())?,
                        // check status of config generation
                        hopr_params::ConfigFileMode::Generated => hopr_config::generate(
                            self.hopr_params.network.clone(),
                            self.hopr_params.rpc_provider.clone(),
                            ticket_value,
                        )?,
                    };
                    let (hopr_startup_notifier_tx, hopr_startup_notifier_rx) = crossbeam_channel::bounded(1);
                    let keys = calc_keys(&self.hopr_params)?;
                    let hoprd = Hopr::new(cfg, keys, hopr_startup_notifier_tx)?;
                    let hopr = Arc::new(hoprd);
                    let node = setup_node(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        hopr.clone(),
                        hopr_startup_notifier_rx.clone(),
                    )?;
                    let metrics = setup_metrics(self.sender.clone(), self.cancel_channel.1.clone(), hopr.clone())?;
                    node.run();
                    metrics.run();

                    Ok(RunMode::Syncing {
                        hopr,
                        node: Box::new(node),
                        metrics: Box::new(metrics),
                        ticket_value,
                    })
                } else {
                    tracing::debug!("no ticket stats: valueing ticket");
                    Ok(self.run_mode.clone())
                }
            }
            RunMode::Syncing {
                hopr,
                node,
                metrics,
                ticket_value,
            } => {
                if hopr.status() == HoprState::Running {
                    tracing::debug!("hopr running: syncing -> full");
                    metrics.cancel();
                    _ = self.cancel_channel.0.send(Cancel::Metrics).map_err(|e| {
                        tracing::error!(%e, "failed to send cancel event to onboarding");
                    });

                    let channel_funding = setup_channel_funding(
                        self.sender.clone(),
                        self.cancel_channel.1.clone(),
                        hopr.clone(),
                        self.config.channel_targets(),
                        ticket_value,
                    )?;

                    channel_funding.run();

                    Ok(RunMode::Full {
                        hopr: hopr.clone(),
                        node: node.clone(),
                        channel_funding: Box::new(channel_funding),
                        ticket_value,
                    })
                } else {
                    tracing::debug!("hopr not running: syncing");
                    Ok(self.run_mode.clone())
                }
            }
            RunMode::Full { .. } => {
                tracing::debug!("full operation");
                Ok(self.run_mode.clone())
            }
        }
    }
*/

/*
pub fn shutdown(&mut self) -> crossbeam_channel::Receiver<()> {
    let (sender, receiver) = crossbeam_channel::bounded(1);
    self.shutdown_sender = Some(sender);
    match &self.run_mode {
        RunMode::Initializing => {}
        RunMode::PreSafe { .. } => {
            tracing::debug!("shutting down from presafe mode");
            _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to onboarding");
            });
            if let Some(s) = self.shutdown_sender.as_ref() {
                _ = s.send(()).map_err(|e| {
                    tracing::error!(%e, "failed to send shutdown complete signal");
                })
            };
        }
        RunMode::ValueingTicket { .. } => {
            tracing::debug!("shutting down from ticket pricing mode");
            _ = self.cancel_channel.0.send(Cancel::ValueingTicket).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing ticket");
            });
        }
        RunMode::Syncing { .. } => {
            tracing::debug!("shutting down from syncing mode");
            _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing ticket");
            });
            _ = self.cancel_channel.0.send(Cancel::Metrics).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing ticket");
            });
            if let Some(s) = self.shutdown_sender.as_ref() {
                _ = s.send(()).map_err(|e| {
                    tracing::error!(%e, "failed to send shutdown complete signal");
                })
            };
        }
        RunMode::Full { .. } => {
            tracing::debug!("shutting down from normal mode");
            _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing ticket");
            });
            _ = self.cancel_channel.0.send(Cancel::ChannelFunding).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing ticket");
            });
            match &mut self.connection {
                Some(conn) => {
                    tracing::info!(current = %conn.destination(), "disconnecting from current destination due to shutdown");
                    self.target_destination = None;
                    conn.dismantle();
                }
                None => {
                    tracing::debug!("direct shutdown - no connection to disconnect");
                    _ = self.cancel_channel.0.send(Cancel::Connection).map_err(|e| {
                        tracing::error!(%e, "failed to send cancel event to connection");
                    });
                    if let Some(s) = self.shutdown_sender.as_ref() {
                        _ = s.send(()).map_err(|e| {
                            tracing::error!(%e, "failed to send shutdown complete signal");
                        })
                    };
                }
            }
        }
    }
    receiver
}
    */

/*
    pub fn handle_cmd(&mut self, cmd: &Command) -> Result<Response, Error> {
        tracing::debug!(%cmd, "handling command");
        let destinations = self.config.destinations.clone();
        match cmd {
            Command::Ping => Ok(Response::Pong),
            Command::Connect(address) => match destinations.get(address) {
                Some(dest) => {
                    self.target_destination = Some(dest.clone());
                    self.act_on_target()?;
                    Ok(Response::connect(command::ConnectResponse::new(dest.clone().into())))
                }
                None => {
                    tracing::info!(address = %address, "cannot connect to destination - address not found");
                    Ok(Response::connect(command::ConnectResponse::address_not_found()))
                }
            },
            Command::Disconnect => {
                self.target_destination = None;
                self.act_on_target()?;
                let dest = self.connection.as_ref().map(|c| c.destination());
                match dest {
                    Some(d) => Ok(Response::disconnect(command::DisconnectResponse::new(d.into()))),
                    None => Ok(Response::disconnect(command::DisconnectResponse::not_connected())),
                }
            }
            Command::Status => {
                let run_mode = match &self.run_mode {
                    RunMode::Initializing => command::RunMode::initializing(),
                    RunMode::PreSafe { node_address, .. } => {
                        let balance = self.presafe_balance.clone().unwrap_or_default();
                        let funding_tool = self.funding_tool.clone();
                        command::RunMode::preparing_safe(*node_address, balance, funding_tool)
                    }
                    RunMode::Syncing { hopr, .. } => {
                        let syncing = self.metrics.clone().map(|m| m.sync_percentage).unwrap_or_default();
                        command::RunMode::warmup(syncing, hopr.status().to_string())
                    }
                    RunMode::ValueingTicket { .. } => command::RunMode::valueing_ticket(),
                    RunMode::Full { hopr, .. } => {
                        let connection_state = match (
                            self.target_destination.clone(),
                            self.connection.clone().map(|c| c.destination()),
                            self.session_connected,
                        ) {
                            (Some(dest), _, true) => command::ConnectionState::connected(dest.clone().into()),
                            (Some(dest), _, false) => command::ConnectionState::connecting(dest.clone().into()),
                            (None, Some(conn_dest), _) => command::ConnectionState::disconnecting(conn_dest.into()),
                            (None, None, _) => command::ConnectionState::disconnected(),
                        };
                        let funding_state: command::FundingState =
                            if let (Some(balance), Some(ticket_stats)) = (&self.balance, &self.ticket_stats) {
                                let ticket_value = ticket_stats.ticket_value()?;
                                balance
                                    .to_funding_issues(self.config.channel_targets().len(), ticket_value)
                                    .into()
                            } else {
                                command::FundingState::Unknown
                            };

                        command::RunMode::running(connection_state, funding_state, hopr.status().to_string())
                    }
                };

                let available_destinations = self
                    .config
                    .destinations
                    .values()
                    .map(|v| {
                        let dest = v.clone();
                        dest.into()
                    })
                    .collect();
                let network = self.hopr_params.network.clone();
                Ok(Response::status(command::StatusResponse::new(
                    run_mode,
                    available_destinations,
                    network,
                )))
            }
            Command::Balance => match (&self.balance, &self.info, &self.ticket_stats) {
                (Some(balance), Some(info), Some(ticket_stats)) => {
                    let ticket_value = ticket_stats.ticket_value()?;
                    let issues: Vec<balance::FundingIssue> =
                        balance.to_funding_issues(self.config.channel_targets().len(), ticket_value);

                    let resp = command::BalanceResponse::new(
                        format!("{} xDai", balance.node_xdai),
                        format!("{} wxHOPR", balance.safe_wxhopr),
                        format!("{} wxHOPR", balance.channels_out_wxhopr),
                        issues,
                        command::Addresses {
                            node: info.node_address,
                            safe: info.safe_address,
                        },
                    );
                    Ok(Response::Balance(Some(resp)))
                }
                _ => Ok(Response::Balance(None)),
            },
            Command::RefreshNode => {
                // TODO
                Ok(Response::Empty)
            }
            Command::FundingTool(secret) => match &self.run_mode {
                RunMode::PreSafe {
                    onboarding,
                    node_address,
                    ..
                } => {
                    self.funding_tool = balance::FundingTool::InProgress;
                    onboarding.fund_address(node_address, secret)?;
                    Ok(Response::Empty)
                }
                _ => {
                    tracing::warn!("funding tool only available during onboarding");
                    Ok(Response::Empty)
                }
            },
        }
    }
*/

/*
pub fn handle_event(&mut self, event: Event) -> Result<(), Error> {
    tracing::debug!(%event, "handling event");
    match event {
        Event::Connection(connection::Event::Connected) => self.on_connected(),
        Event::Connection(connection::Event::Disconnected) => self.on_disconnected(),
        Event::Connection(connection::Event::Broken) => self.on_broken(),
        Event::Connection(connection::Event::Dismantled) => self.on_dismantled(),
        Event::Node(node::Event::Info(info)) => self.on_info(info),
        Event::Node(node::Event::Balance(balance)) => self.on_balance(balance),
        Event::Node(node::Event::BackoffExhausted) => self.on_inoperable_node(),
        Event::Onboarding(onboarding::Event::Balance(balance)) => self.on_onboarding_balance(balance),
        Event::Onboarding(onboarding::Event::SafeModule(safe_module)) => {
            self.on_onboarding_safe_module(safe_module)
        }
        Event::Onboarding(onboarding::Event::BackoffExhausted) => self.on_failed_onboarding(),
        Event::Onboarding(onboarding::Event::FundingTool(res)) => self.on_funding_tool(res),
        Event::ChannelFunding(channel_funding::Event::ChannelFundedOk(address)) => {
            self.on_channel_funded_ok(address)
        }
        Event::ChannelFunding(channel_funding::Event::ChannelNotFunded(address)) => {
            self.on_channel_not_funded(address)
        }
        Event::ChannelFunding(channel_funding::Event::BackoffExhausted) => self.on_failed_channel_funding(),
        Event::ChannelFunding(channel_funding::Event::Done) => self.on_channels_funded(),
        Event::Metrics(metrics::Event::Metrics(val)) => self.on_metrics(val),
        Event::TicketStats(Ok(stats)) => self.on_ticket_stats(stats),
        Event::TicketStats(Err(err)) => self.on_failed_valueing_ticket(),
    }
}
*/

/*
pub fn update_config(&mut self, config_path: &Path) -> Result<(), Error> {
    let config = config::read(config_path)?;
    // update target
    if let Some(dest) = self.target_destination.as_ref() {
        if let Some(new_dest) = config.destinations.get(&dest.address) {
            tracing::debug!(current = %dest, new = %new_dest, "target destination updated");
            self.target_destination = Some(new_dest.clone());
        } else {
            tracing::info!(current = %dest, "clearing target destination - not found in new configuration");
            self.target_destination = None;
        }
    }

    self.config = config;

    // handle existing connection
    if let Some(conn) = &mut self.connection {
        tracing::info!(current = %conn.destination(), "disconnecting from current destination due to configuration update");
        conn.dismantle();
        Ok(())
    } else {
        // recheck run mode
        self.balance = None;
        self.info = None;
        self.cancel_run_mode();
        self.run_mode = self.determine_run_mode()?;
        Ok(())
    }
}
*/

/*
fn check_connect(&mut self, destination: &Destination) -> Result<(), Error> {
    match &self.run_mode {
        RunMode::Initializing => {
            tracing::warn!("edge client not running - waiting to connect");
            Ok(())
        }
        RunMode::PreSafe { .. } => {
            tracing::warn!("edge client not running - waiting to connect");
            Ok(())
        }
        RunMode::ValueingTicket { .. } => {
            tracing::warn!("edge client not ready - waiting to connect");
            Ok(())
        }
        RunMode::Syncing { .. } => {
            tracing::warn!("edge client not ready - waiting to connect");
            Ok(())
        }
        RunMode::Full { hopr, .. } => self.connect(destination, hopr.clone()),
    }
}

fn connect(&mut self, destination: &Destination, hopr: Arc<Hopr>) -> Result<(), Error> {
    let (s, r) = crossbeam_channel::unbounded();
    let config_wireguard = self.config.wireguard.clone();
    let wg = wg_tooling::WireGuard::from_config(config_wireguard)?;
    let config_connection = self.config.connection.clone();
    let mut conn = Connection::new(hopr.clone(), destination.clone(), wg, s, config_connection);
    self.connection = Some(conn.clone());
    let sender = self.sender.clone();
    conn.establish();
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(r) -> conn_event => {
                    match conn_event {
                        Ok(event) => {
                            _ = sender.send(Event::Connection(event)).map_err(|error| {
                                tracing::error!(%event, %error, "failed to send ConnectionEvent event");
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

fn on_connected(&mut self) -> Result<(), Error> {
    tracing::debug!("on connected");
    self.session_connected = true;
    Ok(())
}

fn on_disconnected(&mut self) -> Result<(), Error> {
    self.session_connected = false;
    tracing::info!("connection disconnected - might be network hiccup");
    Ok(())
}

fn on_broken(&mut self) -> Result<(), Error> {
    tracing::warn!("connection broken - attempting to reconnect");
    self.session_connected = false;
    match self.connection.as_mut() {
        Some(conn) => {
            conn.dismantle();
        }
        None => {
            tracing::warn!("received broken event from unreferenced connection");
        }
    }
    Ok(())
}

fn on_dismantled(&mut self) -> Result<(), Error> {
    tracing::info!("connection closed");
    self.connection = None;
    self.session_connected = false;
    _ = self.cancel_channel.0.send(Cancel::Connection).map_err(|e| {
        tracing::error!(%e, "failed to send cancel event to connection");
    });
    if let Some(sender) = self.shutdown_sender.as_ref() {
        tracing::debug!("shutting down after disconnecting");
        _ = sender.send(()).map_err(|e| {
            tracing::error!(%e, "failed to send shutdown complete signal");
        });
        Ok(())
    } else {
        self.act_on_target()
    }
}

fn on_info(&mut self, info: info::Info) -> Result<(), Error> {
    tracing::info!("on info: {info}");
    self.info = Some(info);
    Ok(())
}

fn on_balance(&mut self, balance: balance::Balances) -> Result<(), Error> {
    tracing::info!("on balance: {balance}");
    self.balance = Some(balance);
    Ok(())
}

fn on_inoperable_node(&mut self) -> Result<(), Error> {
    tracing::error!("node is inoperable - please check your configuration and network connectivity");
    self.cancel_run_mode();
    self.run_mode = self.determine_run_mode()?;
    Ok(())
}

fn on_onboarding_balance(&mut self, presafe: balance::PreSafe) -> Result<(), Error> {
    tracing::info!("on presafe balance: {presafe}");
    self.presafe_balance = Some(presafe);
    Ok(())
}

fn on_onboarding_safe_module(&mut self, safe_module: hopr_config::SafeModule) -> Result<(), Error> {
    tracing::info!(?safe_module, "on safe module");
    hopr_config::store_safe(&safe_module)?;
    self.run_mode = self.determine_run_mode()?;
    Ok(())
}

fn on_failed_onboarding(&mut self) -> Result<(), Error> {
    tracing::error!("onboarding failed - please check your configuration and network connectivity");
    self.cancel_run_mode();
    self.run_mode = self.determine_run_mode()?;
    Ok(())
}

fn on_funding_tool(&mut self, res: Result<(), String>) -> Result<(), Error> {
    match res {
        Ok(_) => {
            tracing::info!("funding tool completed successfully");
            self.funding_tool = balance::FundingTool::CompletedSuccess;
        }
        Err(e) => {
            tracing::error!(%e, "funding tool encountered an error");
            self.funding_tool = balance::FundingTool::CompletedError;
        }
    }
    Ok(())
}

fn on_ticket_stats(&mut self, stats: TicketStats) -> Result<(), Error> {
    tracing::debug!(?stats, "received ticket stats");
    self.ticket_stats = Some(stats);
    self.run_mode = self.determine_run_mode()?;
    Ok(())
}

fn on_channel_funded_ok(&mut self, address: Address) -> Result<(), Error> {
    tracing::debug!(address = %address, "channel funded successfully");
    self.funded_channels.push(address);
    Ok(())
}

fn on_channel_not_funded(&mut self, address: Address) -> Result<(), Error> {
    tracing::warn!(address = %address, "channel funding failed");
    self.funded_channels.retain(|&x| x != address);
    Ok(())
}

fn on_failed_channel_funding(&mut self) -> Result<(), Error> {
    tracing::warn!("channel funding failed - please check your RPC provider setting");
    Ok(())
}

fn on_channels_funded(&mut self) -> Result<(), Error> {
    tracing::info!("channel funding completed successfully");
    /*
    _ = self.cancel_channel.0.send(Cancel::ChannelFunding).map_err(|e| {
        tracing::error!(%e, "failed to send cancel event to channel funding");
    });
    */
    Ok(())
}

fn on_metrics(&mut self, metrics: HoprTelemetry) -> Result<(), Error> {
    tracing::debug!(?metrics, "received metrics");
    self.metrics = Some(metrics);
    self.run_mode = self.determine_run_mode()?;
    Ok(())
}

fn on_failed_valueing_ticket(&mut self) -> Result<(), Error> {
    tracing::error!("failed valueing ticket - please check your RPC provider setting");
    // TODO start from scratch
    Ok(())
}
*/

/*
fn cancel_run_mode(&mut self) {
    match self.run_mode.clone() {
        RunMode::Initializing => {}
        RunMode::PreSafe {
            onboarding: _,
            node_address: _,
        } => {
            tracing::debug!("cancel onboarding");
            _ = self.cancel_channel.0.send(Cancel::Onboarding).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to onboarding");
            });
        }
        RunMode::ValueingTicket => {
            tracing::debug!("cancel valueing ticket");
            _ = self.cancel_channel.0.send(Cancel::ValueingTicket).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to valueing_ticket");
            });
        }
        RunMode::Syncing { .. } => {
            tracing::debug!("cancel metrics and node");
            _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to node");
            });
            _ = self.cancel_channel.0.send(Cancel::Metrics).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to metrics");
            });
        }
        RunMode::Full { .. } => {
            tracing::debug!("cancel channel funding and node");
            tracing::debug!("cancel metrics and node");
            _ = self.cancel_channel.0.send(Cancel::Node).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to node");
            });
            _ = self.cancel_channel.0.send(Cancel::ChannelFunding).map_err(|e| {
                tracing::error!(%e, "failed to send cancel event to channel_funding");
            });
        }
    }
}
*/

/*
fn setup_onboarding(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    private_key: ChainKeypair,
    hopr_params: &HoprParams,
    node_address: Address,
) -> Onboarding {
    let (s, r) = crossbeam_channel::unbounded();
    let onboarding = Onboarding::new(
        s,
        private_key,
        hopr_params.rpc_provider.clone(),
        node_address,
        hopr_params.network.clone(),
    );
    let cancel_onboarding = onboarding.clone();
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Onboarding) => {
                            tracing::info!("shutting down onboarding event handler");
                            cancel_onboarding.cancel();
                            break;
                        }
                        Ok(_) => {
                            // ignoring cancel event in onboarding handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "onboarding failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> onboarding_event => {
                    match onboarding_event {
                        Ok(event) => {
                                _ = sender.send(Event::Onboarding(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send onboarding event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    onboarding
}

fn setup_node(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    edgli: Arc<Hopr>,
    hopr_startup_notifier: crossbeam_channel::Receiver<
        std::result::Result<Vec<EdgliProcesses>, edgli::hopr_lib::errors::HoprLibError>,
    >,
) -> Result<Node, Error> {
    let (s, r) = crossbeam_channel::unbounded();
    let node = Node::new(s, edgli.clone());
    let cancel_node = node.clone();
    thread::spawn(move || {
        let mut hopr_processes = Vec::new();

        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Node) => {
                            tracing::info!("shutting down node event handler");
                            cancel_node.cancel();
                            break;
                        }
                        Ok(_) => {
                            // ignoring cancel event in node handler
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "node init failed to receive cancel event");
                        }
                    }
                },
                recv(hopr_startup_notifier) -> startup_result => {
                    match startup_result {
                        Ok(Ok(processes)) => {
                            tracing::info!("HOPR processes started: {:?}", processes);
                            hopr_processes = processes;
                            break
                        }
                        Ok(Err(e)) => {
                            tracing::error!(%e, "failed to start HOPR processes");
                            return;
                        }
                        Err(e) => {
                            tracing::error!(%e, "failed to receive HOPR startup notification");
                            return;
                        }
                    }
                }
            }
        }

        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Node) => {
                            tracing::info!("shutting down node event handler");
                            for process in &mut hopr_processes {
                                tracing::info!("shutting down HOPR process: {process}");
                                match process {
                                    EdgliProcesses::HoprLib(_process, handle) => handle.abort(),
                                    EdgliProcesses::Hopr(handle) => handle.abort(),
                                }
                            }
                            break;
                        }
                        Ok(_) => {
                            // ignoring other cancel event handlers
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "node loop failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> node_event => {
                    match node_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::Node(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send NodeEvent event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    Ok(node)
}

fn setup_channel_funding(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    edgli: Arc<Hopr>,
    channel_targets: Vec<Address>,
    ticket_value: Balance<WxHOPR>,
) -> Result<ChannelFunding, Error> {
    let (s, r) = crossbeam_channel::unbounded();
    let channel_funding = ChannelFunding::new(s, edgli.clone(), channel_targets, ticket_value);
    let cancel_channel_funding = channel_funding.clone();
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::ChannelFunding) => {
                            tracing::info!("shutting down channel funding event handler");
                            cancel_channel_funding.cancel();
                            break;
                        }
                        Ok(_) => {
                            // ignoring other cancel event handlers
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "channel funding loop failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> channel_event => {
                    match channel_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::ChannelFunding(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send ChannelFunding event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    Ok(channel_funding)
}

fn setup_metrics(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    edgli: Arc<Hopr>,
) -> Result<Metrics, Error> {
    let (s, r) = crossbeam_channel::unbounded();
    let metrics = Metrics::new(s, edgli.clone());
    let cancel_metrics = metrics.clone();
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::Metrics) => {
                            tracing::info!("shutting down metrics event handler");
                            cancel_metrics.cancel();
                            break;
                        }
                        Ok(_) => {
                            // ignoring other cancel event handlers
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "metrics loop failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> channel_event => {
                    match channel_event {
                        Ok(ref event) => {
                                _ = sender.send(Event::Metrics(event.clone())).map_err(|error| {
                                    tracing::error!(%event, %error, "failed to send ChannelFunding event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
    Ok(metrics)
}

fn fetch_ticket_stats(
    sender: crossbeam_channel::Sender<Event>,
    cancel_receiver: crossbeam_channel::Receiver<Cancel>,
    private_key: ChainKeypair,
    rpc_provider: Url,
    network: Network,
) {
    let (s, r) = crossbeam_channel::unbounded();
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(TicketStats::fetch(
            &private_key,
            rpc_provider.as_str(),
            &NetworkSpecifications::from_network(&network),
        ));
        s.send(res);
    });
    thread::spawn(move || {
        loop {
            crossbeam_channel::select! {
                recv(cancel_receiver) -> msg => {
                    match msg {
                        Ok(Cancel::ValueingTicket) => {
                            tracing::info!("shutting down one shot tasks event handler");
                            break;
                        }
                        Ok(_) => {
                            // ignoring other cancel event handlers
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "one shot tasks loop failed to receive cancel event");
                        }
                    }
                },
                recv(r) -> ticket_stats => {
                    match ticket_stats {
                        Ok(event) => {
                                let _ = sender.send(Event::TicketStats(event)).map_err(|error| {
                                    tracing::error!(%error, "failed to send ValueingTicket event");
                                });
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to receive event");
                        }
                    }
                },
            }
        }
    });
}
*/

/*
impl Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunMode::Initializing => write!(f, "Initializing"),
            RunMode::PreSafe { .. } => write!(f, "PreSafe"),
            RunMode::ValueingTicket { .. } => write!(f, "ValueingTicket"),
            RunMode::Syncing { .. } => write!(f, "Syncing"),
            RunMode::Full { .. } => write!(f, "Full"),
        }
    }
}

async fn act_on_run_mode(run_mode: RunMode, hopr_params: HoprParams) -> Result<RunMode, Error> {
    match run_mode.clone() {
        RunMode::Initializing => {
            if hopr_config::has_safe() {
                tracing::debug!("safe found: init -> valueing ticket");
                Ok(RunMode::ValueingTicket)
            } else {
                tracing::debug!("safe not found: init -> onboarding");
                let keys = calc_keys(&hopr_params)?;
                let node_address = keys.chain_key.public().to_address();
                unimplemented!("TODO cancel token");
                // Ok(RunMode::PreSafe { node_address })
            }
        }
        // RunMode::PreSafe { node_address } => {}
        RunMode::ValueingTicket => {
            let keys = calc_keys(&hopr_params)?;
            let private_key = keys.chain_key;
            let rpc_provider = hopr_params.rpc_provider.clone();
            let network = hopr_params.network.clone();
            unimplemented!("TODO cancel token");
            /*
            let res = self
                .cancel_for_shutdown_token
                .run_until_cancelled(TicketStats::fetch(
                    &private_key,
                    rpc_provider.as_str(),
                    &NetworkSpecifications::from_network(&network),
                ))
                .await;
            match res {
                Some(Ok(stats)) => {
                    tracing::debug!(?stats, "received ticket attributes");
                    self.ticket_stats = Some(stats);
                    // self.run_mode = RunMode::Syncing;
                }
                Some(Err(err)) => {
                    tracing::error!(%err, "failed fetching ticket attributes");
                    self.ticket_stats = None;
                    self.run_mode = RunMode::ValueingTicket;
                }
                None => {
                    tracing::debug!("cancelled ticket attributes fetching");
                }
            }
            */
        }
        _ => {
            unimplemented!()
        }
    }
}
*/
