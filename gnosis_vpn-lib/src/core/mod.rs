use edgli::EdgliInitState;
use edgli::blokli::SafelessInteractor;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::{Balance, WxHOPR};
use futures_util::future::AbortHandle;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::command::{self, Command, Response, RunMode};
use crate::compat::SafeModule;
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::Destination;
use crate::connectivity_health::{self, ConnectivityHealth};
use crate::destination_health::{self, DestinationHealth};
use crate::event::{CoreToWorker, RequestToRoot, ResponseFromRoot, RunnerToRoot, WorkerToCore};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, config as hopr_config, identity};
use crate::ticket_stats::TicketStats;
use crate::worker_params::{self, WorkerParams};
use crate::{balance, log_output, wireguard};

pub mod runner;

use runner::Results;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("HOPR error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Hopr config error: {0}")]
    HoprConfig(#[from] hopr_config::Error),
    #[error("Hopr identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("Hopr params error: {0}")]
    HoprParams(#[from] worker_params::Error),
    #[error("Safeless Interactor creation error: {0}")]
    SafelessInteractorCreation(String),
}

pub struct Core {
    // config data
    config: Config,

    // static data
    worker_params: WorkerParams,
    node_address: Address,
    safeless_interactor: Arc<SafelessInteractor>,
    outgoing_sender: mpsc::Sender<CoreToWorker>,

    // cancellation tokens
    cancel_balances: CancellationToken,
    cancel_connection: CancellationToken,
    cancel_on_shutdown: CancellationToken,
    cancel_presafe_queries: CancellationToken,

    // user provided data
    target_destination: Option<Destination>,

    // runtime data
    phase: Phase,
    balances: Option<balance::Balances>,
    funding_tool: balance::FundingTool,
    hopr: Option<Arc<Hopr>>,
    ticket_value: Option<Balance<WxHOPR>>,
    strategy_handle: Option<AbortHandle>,
    destination_healths: HashMap<String, DestinationHealth>,
    connectivity_health: HashMap<String, ConnectivityHealth>,
    responder_unit: Option<oneshot::Sender<Result<(), String>>>,
    responder_duration: Option<oneshot::Sender<Result<Duration, String>>>,
    ongoing_disconnections: Vec<connection::down::Down>,
    ongoing_channel_fundings: Vec<Address>,
}

#[derive(Debug, Clone)]
enum Phase {
    Initial,
    CreatingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
        deploy_safe: Querying<SafeModule>,
    },
    DeployingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
    },
    Starting(Option<EdgliInitState>),
    HoprSyncing,
    HoprRunning,
    Connecting(connection::up::Up),
    Connected(connection::up::Up),
    ShuttingDown,
}

#[derive(Debug, Clone)]
enum Querying<T> {
    Init,
    Success(T),
    // TODO expose error in status
    #[allow(dead_code)]
    Error(String),
}

impl Core {
    pub async fn init(
        config: Config,
        worker_params: WorkerParams,
        outgoing_sender: mpsc::Sender<CoreToWorker>,
    ) -> Result<Core, Error> {
        wireguard::available().await?;
        wireguard::executable().await?;
        let keys = worker_params.persist_identity_generation().await?;
        let node_address = keys.chain_key.public().to_address();
        let blokli_config = config.blokli.clone().into();
        let safeless_interactor =
            edgli::blokli::SafelessInteractor::new(worker_params.blokli_url(), &keys.chain_key, Some(blokli_config))
                .await
                .map_err(|e| Error::SafelessInteractorCreation(e.to_string()))?;

        let mut connectivity_health = HashMap::new();
        for (id, dest) in config.destinations.clone() {
            connectivity_health.insert(
                id,
                ConnectivityHealth::from_destination(&dest, worker_params.allow_insecure()),
            );
        }

        let mut destination_healths = HashMap::new();
        for id in config.destinations.keys().cloned() {
            destination_healths.insert(id, DestinationHealth::Init);
        }

        Ok(Core {
            // config data
            config,

            // static data
            worker_params,
            node_address,
            outgoing_sender,

            // cancellation tokens
            cancel_balances: CancellationToken::new(),
            cancel_connection: CancellationToken::new(),
            cancel_on_shutdown: CancellationToken::new(),
            cancel_presafe_queries: CancellationToken::new(),

            // user provided data
            target_destination: None,

            // runtime data
            phase: Phase::Initial,
            balances: None,
            funding_tool: balance::FundingTool::NotStarted,
            hopr: None,
            safeless_interactor: Arc::new(safeless_interactor),
            ticket_value: None,
            strategy_handle: None,
            ongoing_disconnections: Vec::new(),
            ongoing_channel_fundings: Vec::new(),
            connectivity_health,
            destination_healths,
            responder_unit: None,
            responder_duration: None,
        })
    }

    pub async fn start(mut self, incoming_receiver: &mut mpsc::Receiver<WorkerToCore>) {
        let (results_sender, mut results_receiver) = mpsc::channel(32);
        self.initial_runner(&results_sender).await;
        loop {
            tokio::select! {

                // React to an incoming worker events
                Some(event) = incoming_receiver.recv() => {
                    if self.on_event(event, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
                }

                // React to internal results from spawned runner tasks
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

    /// receive an event from the worker main thread
    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    async fn on_event(&mut self, event: WorkerToCore, results_sender: &mpsc::Sender<Results>) -> bool {
        match event {
            WorkerToCore::Shutdown => {
                tracing::debug!("incoming shutdown request");
                self.phase = Phase::ShuttingDown;
                // no need to recreate cancellation tokens after shutdown
                self.cancel_balances.cancel();
                self.cancel_connection.cancel();
                self.cancel_on_shutdown.cancel();
                self.cancel_presafe_queries.cancel();
                if let Some(hopr) = self.hopr.clone() {
                    let shutdown_tracker = TaskTracker::new();
                    if let Some(handle) = self.strategy_handle.take() {
                        shutdown_tracker.spawn(async move {
                            tracing::debug!("aborting strategy task");
                            handle.abort();
                        });
                    }
                    shutdown_tracker.spawn(async move {
                        tracing::debug!("shutting down hopr");
                        hopr.shutdown().await;
                    });
                    shutdown_tracker.close();
                    shutdown_tracker.wait().await;
                }
                false
            }

            WorkerToCore::ResponseFromRoot(resp) => {
                tracing::debug!(?resp, "incoming response from root");
                match resp {
                    ResponseFromRoot::DynamicWgRouting { res } => {
                        if let Some(responder) = self.responder_unit.take() {
                            let _ = responder.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for dynamic wg routing response");
                            });
                        } else {
                            tracing::warn!(?res, "no responder channel available for root response");
                        }
                    }
                    ResponseFromRoot::StaticWgRouting { res } => {
                        if let Some(responder) = self.responder_unit.take() {
                            let _ = responder.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for static wg routing response");
                            });
                        } else {
                            tracing::warn!(?res, "no responder channel available for root response");
                        }
                    }
                    ResponseFromRoot::Ping { res } => {
                        if let Some(responder) = self.responder_duration.take() {
                            let _ = responder.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for ping response");
                            });
                        } else {
                            tracing::warn!(?res, "no responder channel available for root response");
                        }
                    }
                };

                true
            }

            WorkerToCore::Command { cmd, resp } => {
                tracing::debug!(%cmd, "incoming command");
                match cmd {
                    Command::Status => {
                        let runmode = match self.phase.clone() {
                            Phase::Initial => RunMode::Init,
                            Phase::CreatingSafe {
                                node_balance,
                                query_safe: _,
                                deploy_safe,
                            } => {
                                let safe_creation_error = match deploy_safe {
                                    Querying::Error(e) => Some(e.clone()),
                                    _ => None,
                                };
                                let balance = match node_balance {
                                    Querying::Success(b) => Some(b),
                                    _ => None,
                                };
                                RunMode::preparing_safe(
                                    self.node_address,
                                    &balance,
                                    self.funding_tool.clone(),
                                    safe_creation_error,
                                )
                            }
                            Phase::DeployingSafe {
                                node_balance: _,
                                query_safe: _,
                            } => RunMode::deploying_safe(self.node_address),
                            Phase::Starting(edgli_init_state) => RunMode::warmup(edgli_init_state, None),
                            Phase::HoprSyncing => RunMode::warmup(None, self.hopr.as_ref().map(|h| h.status())),
                            Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                                if let (Some(balances), Some(ticket_value)) = (&self.balances, self.ticket_value) {
                                    let min_channel_count = connectivity_health::count_distinct_channels(
                                        &self.connectivity_health.values().collect::<Vec<_>>(),
                                    );
                                    let issues = balances.to_funding_issues(min_channel_count, ticket_value);
                                    RunMode::running(Some(issues), self.hopr.as_ref().map(|h| h.status()))
                                } else {
                                    RunMode::running(None, self.hopr.as_ref().map(|h| h.status()))
                                }
                            }
                            Phase::ShuttingDown => RunMode::Shutdown,
                        };

                        let mut vals = self.config.destinations.values().collect::<Vec<&Destination>>();
                        vals.sort_by(|a, b| a.id.cmp(&b.id));
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
                                    connectivity: self.connectivity_health.get(&v.id).cloned().unwrap_or_default(),
                                    exit_health: self.destination_healths.get(&v.id).cloned().unwrap_or_default(),
                                }
                            })
                            .collect();
                        let res = Response::status(command::StatusResponse::new(runmode, destinations));
                        let _ = resp.send(res);
                    }

                    Command::Connect(id) => match self.config.destinations.clone().get(&id) {
                        Some(dest) => {
                            if let Some(connectivity) = self.connectivity_health.get(&dest.id) {
                                if connectivity.is_ready_to_connect() {
                                    let _ = resp
                                        .send(Response::connect(command::ConnectResponse::connecting(dest.clone())));
                                    self.target_destination = Some(dest.clone());
                                    self.act_on_target(results_sender);
                                } else if connectivity.is_unrecoverable() {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::unable(
                                        dest.clone(),
                                        connectivity.clone(),
                                    )));
                                } else {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::waiting(
                                        dest.clone(),
                                        connectivity.clone(),
                                    )));
                                    self.target_destination = Some(dest.clone());
                                }
                            } else {
                                tracing::warn!(%id, "no connectivity health found for destination - this should not happen");
                                let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                            }
                        }
                        None => {
                            tracing::info!(%id, "cannot connect to destination - not configured");
                            let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
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
                            let res = command::BalanceResponse::new(
                                &hopr.info(),
                                &balances,
                                &ticket_value,
                                &self.config.destinations.clone(),
                                self.connectivity_health.values().collect::<Vec<_>>().as_slice(),
                                self.ongoing_channel_fundings.iter().collect::<Vec<_>>().as_slice(),
                            );
                            let _ = resp.send(Response::Balance(Some(res)));
                        } else {
                            let _ = resp.send(Response::Balance(None));
                        }
                    }

                    Command::Ping => {
                        let _ = resp.send(Response::Pong);
                    }

                    Command::Telemetry => {
                        let res = match hopr::telemetry() {
                            Ok(t) => Some(t),
                            Err(err) => {
                                tracing::error!(?err, "failed to collect hopr telemetry");
                                None
                            }
                        };
                        let _ = resp.send(Response::Telemetry(res));
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
                        let metrics = match edgli::hopr_lib::Hopr::<bool, bool>::collect_hopr_metrics() {
                            Ok(m) => m,
                            Err(err) => {
                                tracing::error!(?err, "failed to collect hopr metrics");
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

    /// Results are events from async runners
    #[tracing::instrument(skip(self, results_sender, results), level = "debug", ret)]
    async fn on_results(&mut self, results: Results, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(phase = ?self.phase, %results, "on runner results");
        match results {
            Results::HoprConstruction(edgli_state) => {
                if matches!(self.phase, Phase::Starting(_)) {
                    self.phase = Phase::Starting(Some(edgli_state));
                } else {
                    tracing::warn!(?self.phase, "hopr construction result received in unexpected phase");
                }
            }
            Results::TicketStats { res } => match res {
                Ok(stats) => self.on_ticket_stats(stats, results_sender),
                Err(err) => {
                    tracing::error!(?err, "failed to fetch ticket stats - retrying");
                    self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::NodeBalance { res } => self.on_results_node_balance(res, results_sender).await,
            Results::QuerySafe { res } => self.on_results_query_safe(res, results_sender).await,
            Results::DeploySafe { res } => self.on_results_deploy_safe(res, results_sender).await,

            Results::PersistSafe { res, safe_module } => match res {
                Ok(()) => {
                    tracing::info!("safe module persisted");
                }
                Err(err) => {
                    tracing::error!(?err, "failed to persist safe module - retrying");
                    self.spawn_store_safe(safe_module, results_sender, Duration::from_secs(10));
                }
            },

            Results::Hopr { res, safe_module } => match res {
                Ok(hopr) => {
                    tracing::info!("hopr runner started successfully");
                    self.phase = Phase::HoprSyncing;
                    self.hopr = Some(Arc::new(hopr));
                    self.ticket_value = None;
                    self.spawn_balances_runner(results_sender, Duration::ZERO);
                    self.spawn_ticket_stats_runner(results_sender, Duration::ZERO);
                    self.spawn_wait_for_running(results_sender, Duration::from_secs(1));
                }
                Err(err) => {
                    tracing::error!(?err, "hopr runner failed to start - trying again in 10 seconds");
                    self.spawn_hopr_runner(safe_module, results_sender, Duration::from_secs(10));
                }
            },

            Results::FundingTool { res } => match res {
                Ok(None) => self.funding_tool = balance::FundingTool::CompletedSuccess,
                Ok(Some(reason)) => self.funding_tool = balance::FundingTool::CompletedError(reason),
                Err(err) => {
                    tracing::error!(?err, "funding runner exited with error");
                    self.funding_tool = balance::FundingTool::CompletedError(err.to_string());
                }
            },

            Results::Balances { res } => match res {
                Ok(balances) => {
                    tracing::info!(%balances, "received balances from hopr");
                    self.balances = Some(balances);
                    self.spawn_balances_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch balances from hopr");
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
                    for (target, health) in self.connectivity_health.clone() {
                        let updated_health = health.peers(&all_peers);
                        // only spawn channel funding when we are peered
                        if let Some(addr) = updated_health.needs_channel_funding()
                            && !updated_health.needs_peer()
                        {
                            self.spawn_channel_funding(addr, results_sender, Duration::ZERO);
                        }
                        self.connectivity_health.insert(target, updated_health);
                    }

                    let delay =
                        if connectivity_health::needs_peers(&self.connectivity_health.values().collect::<Vec<_>>()) {
                            Duration::from_secs(10)
                        } else {
                            Duration::from_secs(90)
                        };
                    self.spawn_connected_peers(results_sender, delay);
                    self.act_on_target(results_sender);
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch connected peers");
                    self.spawn_connected_peers(results_sender, Duration::from_secs(10));
                }
            },

            Results::FundChannel { address, res } => {
                self.ongoing_channel_fundings.retain(|a| a != &address);
                let destinations = self
                    .config
                    .destinations
                    .iter()
                    .filter_map(|(_, d)| {
                        if d.has_intermediate_channel(address) {
                            Some(d.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                match res {
                    Ok(()) => {
                        tracing::info!(%address, "channel funded");
                        for d in destinations.iter() {
                            self.update_health(d.id.clone(), |h| h.channel_funded(address));
                        }
                        self.act_on_target(results_sender);
                    }
                    Err(err) => {
                        tracing::error!(?err, %address, "failed to ensure channel funding");
                        for d in destinations.iter() {
                            self.update_health(d.id.clone(), |h| h.with_error(err.to_string()));
                        }
                    }
                }
            }

            Results::ConnectionEvent(evt) => {
                tracing::debug!(%evt, "handling connection runner event");
                match self.phase.clone() {
                    Phase::Connecting(mut conn) => match evt {
                        connection::up::Event::Progress(e) => {
                            conn.connect_progress(e);
                            self.phase = Phase::Connecting(conn);
                        }
                        connection::up::Event::Setback(e) => {
                            self.update_health(conn.destination.id, |h| h.with_error(e.to_string()));
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
                if matches!(evt, connection::down::Event::OpenBridge) {
                    self.act_on_target(results_sender);
                }
            }

            Results::ConnectionResult { res } => match (res, self.phase.clone()) {
                (Ok(session), Phase::Connecting(mut conn)) => {
                    tracing::info!(%conn, "connection established successfully");
                    conn.connected();
                    self.phase = Phase::Connected(conn.clone());
                    self.update_health(conn.destination.id.clone(), |h| h.no_error());
                    let route = format!(
                        "{}({})",
                        conn.destination.pretty_print_path(),
                        log_output::address(&conn.destination.address)
                    );
                    log_output::print_session_established(route.as_str());
                    self.spawn_session_monitoring(session, results_sender);
                }
                (Ok(_), phase) => {
                    tracing::warn!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(%conn, ?err, "connection failed");
                    self.update_health(conn.destination.id.clone(), |h| h.with_error(err.to_string()));
                    if let Some(dest) = self.target_destination.clone()
                        && dest == conn.destination
                    {
                        tracing::info!(%dest, "disconnecting from target destination due to connection error");
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
                        tracing::error!(%wg_public_key, ?err, "disconnection failed");
                    }
                }
                self.ongoing_disconnections.retain(|c| c.wg_public_key != wg_public_key);
                self.act_on_target(results_sender);
            }

            Results::SessionMonitorFailed => match self.phase.clone() {
                Phase::Connected(conn) => {
                    tracing::warn!(%conn, "session monitor failed - reconnecting");
                    self.disconnect_from_connection(&conn, results_sender);
                }
                phase => {
                    tracing::error!(?phase, "session monitor failed in unexpected phase");
                }
            },

            Results::ConnectionRequestToRoot(respondable_request) => match respondable_request {
                RunnerToRoot::DynamicWgRouting { wg_data, resp } => {
                    self.responder_unit = Some(resp);
                    let request = RequestToRoot::DynamicWgRouting { wg_data };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::StaticWgRouting {
                    wg_data,
                    peer_ips,
                    resp,
                } => {
                    self.responder_unit = Some(resp);
                    let request = RequestToRoot::StaticWgRouting { wg_data, peer_ips };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::Ping { options, resp } => {
                    self.responder_duration = Some(resp);
                    let request = RequestToRoot::Ping { options };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::TearDownWg => {
                    let request = RequestToRoot::TearDownWg;
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }
            },

            Results::HealthCheck { id, health } => {
                tracing::info!(%id, %health, "received health check");
                let res_next_interval = health.next_interval(matches!(self.phase, Phase::Connected(_)).into());
                self.destination_healths.insert(id.clone(), health);
                if let (Some(int), Some(dest)) = (res_next_interval, self.config.destinations.get(&id)) {
                    self.spawn_health_check_runner(dest.clone(), results_sender, int);
                }
            }
        }
    }

    async fn on_results_node_balance(
        &mut self,
        res: Result<balance::PreSafe, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (
                Ok(presafe),
                Phase::CreatingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe,
                },
            ) => {
                tracing::info!(%presafe, "on presafe node balance");
                self.phase = Phase::CreatingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe,
                    deploy_safe,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CreatingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe,
                },
            ) => {
                tracing::error!(?err, "failed to fetch presafe node balance - retrying");
                self.phase = Phase::CreatingSafe {
                    node_balance: Querying::Error(err.to_string()),
                    query_safe,
                    deploy_safe,
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?phase, ?res, "ignoring presafe node balance result in unexpected phase");
            }
        }
    }

    async fn on_results_query_safe(
        &mut self,
        res: Result<Option<SafeModule>, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (Ok(Some(safe_module)), Phase::CreatingSafe { .. }) => {
                tracing::info!(?safe_module, "found safe module");
                self.cancel_presafe_queries.cancel();
                self.cancel_presafe_queries = CancellationToken::new();
                // start edge client with queried safe module
                self.spawn_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
                // try persisting safe module to disk - might fail but we consider this non critical
                self.spawn_store_safe(safe_module, results_sender, Duration::ZERO);
            }
            (
                Ok(None),
                Phase::CreatingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe,
                },
            ) => {
                tracing::info!("found no deployed safe module");
                self.phase = Phase::CreatingSafe {
                    node_balance,
                    query_safe: Querying::Success(None),
                    deploy_safe,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CreatingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe,
                },
            ) => {
                tracing::error!(?err, "failed to query safe module - retrying");
                self.phase = Phase::CreatingSafe {
                    node_balance,
                    query_safe: Querying::Error(err.to_string()),
                    deploy_safe,
                };
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?phase, ?res, "ignoring query safe result in unexpected phase");
            }
        }
    }

    async fn on_results_deploy_safe(
        &mut self,
        res: Result<SafeModule, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (Ok(safe_module), Phase::DeployingSafe { .. }) => {
                tracing::info!(?safe_module, "deployed safe module");
                // start edge client with new safe module
                self.spawn_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
                // try persisting safe module to disk - might fail but we consider this non critical
                self.spawn_store_safe(safe_module, results_sender, Duration::ZERO);
            }
            (
                Err(err),
                Phase::DeployingSafe {
                    node_balance,
                    query_safe,
                },
            ) => {
                tracing::error!(?err, "failed to deploy safe module - retrying from balance check");
                self.phase = Phase::CreatingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe: Querying::Error(err.to_string()),
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?phase, ?res, "ignoring deploy safe result in unexpected phase");
            }
        }
    }

    fn trigger_deploy_safe(&mut self, results_sender: &mpsc::Sender<Results>) {
        if let Phase::CreatingSafe {
            node_balance: Querying::Success(presafe),
            query_safe: Querying::Success(None),
            deploy_safe: _,
        } = self.phase.clone()
        {
            if presafe.node_xdai.is_zero() || presafe.node_wxhopr.is_zero() {
                tracing::warn!("insufficient funds to start safe deployment - waiting for funding");
            } else {
                self.phase = Phase::DeployingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe: Querying::Success(None),
                };
                self.cancel_presafe_queries.cancel();
                self.cancel_presafe_queries = CancellationToken::new();
                self.spawn_safe_deployment_runner(&presafe, results_sender);
            }
        }
    }

    async fn initial_runner(&mut self, results_sender: &mpsc::Sender<Results>) {
        let res = hopr_config::read_safe(self.worker_params.state_home()).await;
        match res {
            Ok(safe_module) => {
                tracing::debug!(?safe_module, "found existing safe module - starting hopr runner");
                // start edge client with existing safe module
                self.spawn_hopr_runner(safe_module, results_sender, Duration::ZERO);
            }
            Err(err) => {
                if matches!(err, hopr_config::Error::NoFile) {
                    tracing::info!("no persisted safe module found - querying safeless interactor");
                } else {
                    tracing::warn!(
                        ?err,
                        "error deserializing existing safe module - querying safeless interactor"
                    );
                }
                self.phase = Phase::CreatingSafe {
                    node_balance: Querying::Init,
                    query_safe: Querying::Init,
                    deploy_safe: Querying::Init,
                };
                self.spawn_query_safe_runner(results_sender, Duration::ZERO);
                self.spawn_node_balance_runner(results_sender, Duration::ZERO);
            }
        }
    }

    fn spawn_query_safe_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let safeless_interactor = self.safeless_interactor.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::query_safe(safeless_interactor, results_sender).await
                })
                .await
        });
    }

    fn spawn_node_balance_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let safeless_interactor = self.safeless_interactor.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::node_balance(safeless_interactor, results_sender).await
                })
                .await
        });
    }

    fn spawn_funding_runner(&self, secret: String, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move { runner::funding_tool(worker_params, secret, results_sender).await })
                .await;
        });
    }

    fn spawn_safe_deployment_runner(&self, presafe: &balance::PreSafe, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_on_shutdown.clone();
        let safeless_interactor = self.safeless_interactor.clone();
        let presafe = presafe.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::safe_deployment(safeless_interactor, presafe, results_sender).await;
                })
                .await
        });
    }

    fn spawn_store_safe(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let state_home = self.worker_params.state_home();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::persist_safe(state_home, safe_module, results_sender).await;
                })
                .await
        });
    }

    fn spawn_hopr_runner(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        self.phase = Phase::Starting(None);
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::hopr(worker_params, blokli_config, &safe_module, results_sender).await;
                })
                .await
        });
    }

    fn spawn_ticket_stats_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let safeless_interactor = self.safeless_interactor.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::ticket_stats(safeless_interactor, results_sender).await;
                })
                .await
        });
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
            let cancel = self.cancel_on_shutdown.clone();
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
    fn spawn_channel_funding(&mut self, address: Address, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if self.ongoing_channel_fundings.contains(&address) {
            tracing::debug!(%address, "channel funding already ongoing - skipping");
            return;
        }
        self.ongoing_channel_fundings.push(address);
        tracing::debug!(ticket_value = ?self.ticket_value, hopr_present  = self.hopr.is_some(), "checking channel funding");
        if let (Some(hopr), Some(ticket_value)) = (self.hopr.clone(), self.ticket_value) {
            let cancel = self.cancel_on_shutdown.clone();
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

    fn spawn_connected_peers(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
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

    fn spawn_health_check_runner(
        &self,
        destination: Destination,
        results_sender: &mpsc::Sender<Results>,
        delay: Duration,
    ) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            let config_connection = self.config.connection.clone();
            let old_health = self
                .destination_healths
                .get(&destination.id)
                .cloned()
                .unwrap_or_default();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        let runner = destination_health::Runner::new(
                            destination.clone(),
                            config_connection,
                            old_health,
                            hopr.clone(),
                        );
                        runner.start(results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connection_runner(&mut self, destination: Destination, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let conn = connection::up::Up::new(destination.clone());
            let config_connection = self.config.connection.clone();
            let config_wireguard = self.config.wireguard.clone();
            let hopr = hopr.clone();
            let runner = connection::up::runner::Runner::new(
                conn.destination.clone(),
                config_connection,
                config_wireguard,
                hopr,
                self.worker_params.clone(),
            );
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
            let cancel = self.cancel_on_shutdown.clone();
            let config_connection = self.config.connection.clone();
            let hopr = hopr.clone();
            let runner = connection::down::runner::Runner::new(disconn.clone(), hopr, config_connection);
            let results_sender = results_sender.clone();
            self.ongoing_disconnections.push(disconn.clone());
            let outgoing_sender = self.outgoing_sender.clone();
            tokio::spawn(async move {
                // this is a oneshot command and we do not wait for any result
                let _ = outgoing_sender
                    .send(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
                    .await;
                cancel
                    .run_until_cancelled(async move {
                        runner.start(results_sender).await;
                    })
                    .await;
            });
        }
    }

    fn spawn_session_monitoring(&self, session: SessionClientMetadata, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner::monitor_session(hopr, &session, results_sender).await;
                    })
                    .await
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
                if let Some(health) = self.connectivity_health.get(&dest.id) {
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
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while connected");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Connecting to different destination while already connecting
            (Some(dest), Phase::Connecting(conn)) if dest != conn.destination => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while already connecting");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Disconnecting from established connection
            (None, Phase::Connected(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from destination");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Disconnecting while establishing connection
            (None, Phase::Connecting(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from ongoing connection attempt");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // No action needed
            _ => {}
        }
    }

    fn disconnect_from_connection(&mut self, conn: &connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        self.cancel_connection.cancel();
        self.cancel_connection = CancellationToken::new();
        self.phase = Phase::HoprRunning;
        if let Ok(disconn) = conn.try_into() {
            self.spawn_disconnection_runner(&disconn, results_sender);
        } else {
            // connection did not even generate a wg pub key - so we can immediately try to connect again
            self.act_on_target(results_sender);
        }
    }

    fn on_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        if connectivity_health::needs_peers(&self.connectivity_health.values().collect::<Vec<_>>()) {
            self.spawn_connected_peers(results_sender, Duration::ZERO);
        }
        let mut delay = Duration::from_millis(133);
        for (_id, destination) in self.config.destinations.clone() {
            // delay initial health checks a bit
            self.spawn_health_check_runner(destination.clone(), results_sender, delay);
            delay += Duration::from_millis(133);
        }
        self.act_on_target(results_sender);
    }

    fn on_ticket_stats(&mut self, stats: TicketStats, results_sender: &mpsc::Sender<Results>) {
        tracing::info!("received ticket stats from runner");
        match (stats.ticket_value(), self.hopr.as_ref()) {
            (Ok(tv), Some(edgli)) => {
                tracing::info!(%stats, %tv, "determined ticket value from stats");
                self.ticket_value = Some(tv);
                match edgli.start_telemetry_reactor(tv) {
                    Ok(strategy_process) => {
                        tracing::info!("started edge node telemetry reactor");
                        self.strategy_handle = Some(strategy_process);
                    }
                    Err(err) => {
                        tracing::error!(
                            ?err,
                            "failed to start edge node telemetry reactor - retrying ticket stats"
                        );
                        self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
                    }
                }
            }
            (Ok(_), None) => {
                tracing::error!("edgeclient not available when starting telemetry reactor");
            }
            (Err(err), _) => {
                tracing::error!(%stats, ?err, "failed to determine ticket value from stats - retrying");
                self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
            }
        }
    }

    fn update_health<F>(&mut self, id: String, cb: F) -> bool
    where
        F: Fn(&ConnectivityHealth) -> ConnectivityHealth,
    {
        if let Some(health) = self.connectivity_health.get(&id) {
            self.connectivity_health.insert(id, cb(health));
            true
        } else {
            tracing::warn!(?id, "connection destination has no health tracker");
            false
        }
    }
}
