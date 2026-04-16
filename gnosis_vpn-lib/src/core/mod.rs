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

use crate::command::{self, Response, RunMode, WorkerCommand};
use crate::compat::SafeModule;
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::Destination;
use crate::event::{CoreToWorker, RequestToRoot, ResponseFromRoot, RunnerToRoot, WorkerToCore};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, config as hopr_config, identity};
use crate::route_health::{self, RouteHealth};
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
    outgoing_sender: mpsc::Sender<CoreToWorker>,
    incoming_receiver: mpsc::Receiver<WorkerToCore>,

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
    safeless_interactor: Option<Arc<SafelessInteractor>>,
    hopr: Option<Arc<Hopr>>,
    ticket_value: Option<Balance<WxHOPR>>,
    strategy_handle: Option<AbortHandle>,
    route_healths: HashMap<String, RouteHealth>,
    responder_unit: Option<oneshot::Sender<Result<(), String>>>,
    responder_duration: Option<oneshot::Sender<Result<Duration, String>>>,
    ongoing_disconnections: Vec<connection::down::Down>,
    ongoing_channel_fundings: Vec<Address>,
}

#[derive(Debug, Clone)]
enum Phase {
    // initial phase - instantiate safeless interactor (blokli)
    // and determine if safe is present
    Initial,
    /// safe absent or safe deployment error - repeatedly query node balance and safe info
    CheckingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
        funding_tool: balance::FundingTool,
        deploy_safe_error: Option<String>,
    },
    /// enough funds and no deployed safe - run safe deployment
    DeployingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
    },
    /// construct edge client
    Starting(Option<EdgliInitState>),
    /// start edge client
    HoprSyncing,
    /// edge client running normally
    HoprRunning,
    // connecting to a destination
    Connecting(connection::up::Up),
    /// connected to a destination
    Connected(connection::up::Up),
    /// dismantle state
    ShuttingDown,
}

#[derive(Debug, Clone)]
enum Querying<T> {
    Init,
    Success(T),
    Error(String),
}

impl Core {
    pub async fn init(
        config: Config,
        worker_params: WorkerParams,
        target_dest_id: Option<String>,
        outgoing_sender: mpsc::Sender<CoreToWorker>,
    ) -> Result<(Core, mpsc::Sender<WorkerToCore>), Error> {
        wireguard::available().await?;
        wireguard::executable().await?;
        let keys = worker_params.persist_identity_generation().await?;
        let node_address = keys.chain_key.public().to_address();
        let cancel_on_shutdown = CancellationToken::new();
        let mut route_healths = HashMap::new();
        for (id, dest) in config.destinations.clone() {
            route_healths.insert(
                id,
                RouteHealth::new(&dest, worker_params.allow_insecure(), cancel_on_shutdown.clone()),
            );
        }

        let target_destination = target_dest_id.and_then(|id| config.destinations.get(&id).cloned());

        let (incoming_sender, incoming_receiver) = mpsc::channel(32);
        let core = Core {
            // config data
            config,

            // static data
            worker_params,
            node_address,
            outgoing_sender,
            incoming_receiver,

            // cancellation tokens
            cancel_balances: CancellationToken::new(),
            cancel_connection: CancellationToken::new(),
            cancel_on_shutdown: cancel_on_shutdown.clone(),
            cancel_presafe_queries: CancellationToken::new(),

            // user provided data
            target_destination,

            // runtime data
            phase: Phase::Initial,
            balances: None,
            hopr: None,
            safeless_interactor: None,
            ticket_value: None,
            strategy_handle: None,
            ongoing_disconnections: Vec::new(),
            ongoing_channel_fundings: Vec::new(),
            route_healths,
            responder_unit: None,
            responder_duration: None,
        };
        Ok((core, incoming_sender))
    }

    pub async fn start(mut self) {
        let (results_sender, mut results_receiver) = mpsc::channel(32);
        self.spawn_initial_runner(&results_sender, Duration::ZERO);
        loop {
            tokio::select! {
                // React to an incoming worker events
                Some(event) = self.incoming_receiver.recv() => {
                    if self.on_event(event, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
                }

                // React to internal results from spawned runner tasks
                Some(results) = results_receiver.recv() => {
                    if self.on_results(results, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
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

            WorkerToCore::WorkerCommand { cmd, resp } => {
                tracing::debug!(%cmd, "incoming command");
                match cmd {
                    WorkerCommand::NerdStats => {
                        tracing::debug!("incoming nerd stats request");
                        match &self.phase {
                            Phase::Connecting(conn) => {
                                let res = Response::nerd_stats(command::NerdStatsResponse::Connecting(
                                    command::ConnStats::from_conn(conn, self.node_address),
                                ));
                                let _ = resp.send(res);
                            }
                            Phase::Connected(conn) => {
                                let res = Response::nerd_stats(command::NerdStatsResponse::Connected(
                                    command::ConnStats::from_conn(conn, self.node_address),
                                ));
                                let _ = resp.send(res);
                            }
                            _ => {
                                let res = Response::nerd_stats(command::NerdStatsResponse::NoInfo);
                                let _ = resp.send(res);
                            }
                        }
                    }

                    WorkerCommand::Status => {
                        let runmode = match self.phase.clone() {
                            Phase::Initial => RunMode::Init,
                            Phase::CheckingSafe {
                                node_balance,
                                query_safe,
                                funding_tool,
                                deploy_safe_error,
                            } => {
                                let balance = match node_balance {
                                    Querying::Success(ref b) => Some(b.clone()),
                                    _ => None,
                                };
                                let mut errors = "".to_string();
                                if let Querying::Error(err) = node_balance {
                                    errors = err
                                };
                                if let Querying::Error(err) = query_safe {
                                    errors = format!("{} {}", errors, err);
                                }
                                if let Some(deploy_err) = deploy_safe_error {
                                    errors = format!("{} {}", errors, deploy_err);
                                }
                                let funding_tool = match funding_tool {
                                    balance::FundingTool::NotStarted => None,
                                    balance::FundingTool::InProgress => Some("Funding tool running".to_string()),
                                    balance::FundingTool::CompletedSuccess => {
                                        Some("Funding tool ran successfully".to_string())
                                    }
                                    balance::FundingTool::CompletedError(error) => {
                                        Some(format!("Funding tool error: {error}"))
                                    }
                                };
                                let error = if errors.is_empty() { None } else { Some(errors) };
                                RunMode::preparing_safe(
                                    self.node_address,
                                    &balance,
                                    funding_tool,
                                    error,
                                    self.ticket_value,
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
                                    let min_channel_count =
                                        route_health::count_distinct_channels(self.route_healths.values());
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
                                    route_health: self
                                        .route_healths
                                        .get(&v.id)
                                        .map(command::RouteHealthView::from)
                                        // should never be here - mark unrecoverable to indicate misconfiguration
                                        .unwrap_or_else(|| command::RouteHealthView {
                                            state: route_health::RouteHealthState::Unrecoverable {
                                                reason: route_health::UnrecoverableReason::InvalidId,
                                            },
                                            last_error: None,
                                            checking_since: None,
                                            consecutive_failures: 0,
                                        }),
                                }
                            })
                            .collect();
                        let res = Response::status(command::StatusResponse::new(runmode, destinations));
                        let _ = resp.send(res);
                    }

                    WorkerCommand::Connect(id) => match self.config.destinations.clone().get(&id) {
                        Some(dest) => {
                            if let Some(rh) = self.route_healths.get(&dest.id) {
                                if rh.is_ready_to_connect() {
                                    let _ = resp
                                        .send(Response::connect(command::ConnectResponse::connecting(dest.clone())));
                                    self.target_destination = Some(dest.clone());
                                    self.act_on_target(results_sender);
                                } else if rh.is_unrecoverable() {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::unable(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                } else {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::waiting(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                    self.target_destination = Some(dest.clone());
                                }
                            } else {
                                tracing::warn!(%id, "no route health found for destination - this should not happen");
                                let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                            }
                        }
                        None => {
                            tracing::info!(%id, "cannot connect to destination - not configured");
                            let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                        }
                    },

                    WorkerCommand::Disconnect => {
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

                    WorkerCommand::Balance => {
                        if let (Some(hopr), Some(balances), Some(ticket_value)) =
                            (self.hopr.clone(), self.balances.clone(), self.ticket_value)
                        {
                            let res = command::BalanceResponse::new(
                                &hopr.info(),
                                &balances,
                                &ticket_value,
                                &self.config.destinations.clone(),
                                self.route_healths.values().collect::<Vec<_>>().as_slice(),
                                self.ongoing_channel_fundings.iter().collect::<Vec<_>>().as_slice(),
                            );
                            let _ = resp.send(Response::Balance(Some(res)));
                        } else {
                            let _ = resp.send(Response::Balance(None));
                        }
                    }

                    WorkerCommand::Telemetry => {
                        let res = match hopr::telemetry() {
                            Ok(t) => Some(t),
                            Err(err) => {
                                tracing::error!(?err, "failed to collect hopr telemetry");
                                None
                            }
                        };
                        let _ = resp.send(Response::Telemetry(res));
                    }

                    WorkerCommand::RefreshNode => {
                        // immediately request balances and cancel existing balance loop
                        self.cancel_balances.cancel();
                        self.cancel_balances = CancellationToken::new();
                        self.spawn_balances_runner(results_sender, Duration::ZERO);
                        let _ = resp.send(Response::RefreshNodeTriggered);
                    }

                    WorkerCommand::FundingTool(secret) => match self.phase.clone() {
                        Phase::CheckingSafe {
                            node_balance,
                            query_safe,
                            funding_tool,
                            deploy_safe_error,
                        } => match funding_tool {
                            balance::FundingTool::NotStarted | balance::FundingTool::CompletedError(_) => {
                                self.phase = Phase::CheckingSafe {
                                    node_balance,
                                    query_safe,
                                    funding_tool: balance::FundingTool::InProgress,
                                    deploy_safe_error,
                                };
                                self.spawn_funding_runner(secret, results_sender);
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::Started));
                            }
                            balance::FundingTool::InProgress => {
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::InProgress));
                            }
                            balance::FundingTool::CompletedSuccess => {
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::Done));
                            }
                        },
                        _ => {
                            let _ = resp.send(Response::funding_tool(command::FundingToolResponse::WrongPhase));
                        }
                    },
                }
                true
            }
        }
    }

    /// Results are events from async runners
    #[tracing::instrument(skip(self, results_sender, results), level = "debug", ret)]
    async fn on_results(&mut self, results: Results, results_sender: &mpsc::Sender<Results>) -> bool {
        tracing::debug!(%results, phase = ?self.phase, "on runner results");
        match results {
            Results::SafelessInteractor { res } => self.on_results_safeless_interactor(res, results_sender).await,
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
            Results::FundingTool { res } => self.on_results_funding_tool(res),

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
                    self.spawn_balances_runner(results_sender, Duration::ZERO);
                    self.try_start_reactor(results_sender);
                    self.spawn_wait_for_running(results_sender, Duration::from_secs(1));
                }
                Err(err) => {
                    tracing::error!(?err, "hopr runner failed to start - trying again in 10 seconds");
                    self.spawn_hopr_runner(safe_module, results_sender, Duration::from_secs(10));
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
                    let dest_ids: Vec<String> = self.route_healths.keys().cloned().collect();
                    for id in dest_ids {
                        if let Some(dest) = self.config.destinations.get(&id).cloned()
                            && let Some(rh) = self.route_healths.get_mut(&id)
                        {
                            let transition = rh.peers(
                                &all_peers,
                                self.hopr.as_ref().unwrap(),
                                &dest,
                                &self.config.connection,
                                results_sender,
                            );
                            match transition {
                                route_health::PeerTransition::NowNeedsFunding => {
                                    if let Some(addr) = rh.needs_channel_funding() {
                                        self.spawn_channel_funding(addr, results_sender, Duration::ZERO);
                                    }
                                }
                                route_health::PeerTransition::BecameRoutable => {}
                                route_health::PeerTransition::LostPeer => {}
                                route_health::PeerTransition::NoChange => {}
                            }
                        }
                    }

                    let delay = if route_health::any_needs_peers(self.route_healths.values()) {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_secs(90)
                    };
                    self.spawn_connected_peers(results_sender, delay);
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch connected peers");
                    self.spawn_connected_peers(results_sender, Duration::from_secs(10));
                }
            },

            Results::FundChannel { address, res } => {
                self.ongoing_channel_fundings.retain(|a| a != &address);
                let dest_ids: Vec<String> = self
                    .config
                    .destinations
                    .iter()
                    .filter_map(|(_, d)| {
                        if d.has_intermediate_channel(address) {
                            Some(d.id.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                match res {
                    Ok(()) => {
                        tracing::info!(address = %address.to_checksum(), "channel funded");
                        for id in &dest_ids {
                            if let (Some(rh), Some(dest)) =
                                (self.route_healths.get_mut(id), self.config.destinations.get(id))
                            {
                                rh.channel_funded(
                                    address,
                                    self.hopr.as_ref().unwrap(),
                                    dest,
                                    &self.config.connection,
                                    results_sender,
                                );
                            }
                        }
                    }
                    Err(err) => {
                        tracing::error!(?err, address = %address.to_checksum(), "failed to ensure channel funding");
                        for id in &dest_ids {
                            if let Some(rh) = self.route_healths.get_mut(id) {
                                rh.with_error(err.to_string());
                            }
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
                            if let Some(rh) = self.route_healths.get_mut(&conn.destination.id) {
                                rh.with_error(e.to_string());
                            }
                        }
                    },
                    phase => {
                        tracing::warn!(%evt, ?phase, "received connection event in unexpected phase");
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
                    tracing::warn!(%evt, ?self.phase, "received disconnection event for unknown connection");
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
                    let route = format!(
                        "{}({})",
                        conn.destination.pretty_print_path(),
                        log_output::address(&conn.destination.address)
                    );
                    log_output::print_session_established(route.as_str());
                    self.spawn_session_monitoring(session, results_sender);
                    self.spawn_tunnel_ping_probe(results_sender);
                }
                (Ok(_), phase) => {
                    tracing::warn!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(?err, %conn, "connection failed");
                    if let Some(rh) = self.route_healths.get_mut(&conn.destination.id) {
                        rh.with_error(err.to_string());
                    }
                    if let Some(dest) = self.target_destination.clone()
                        && dest == conn.destination
                    {
                        tracing::info!(%dest, "restarting connection worker process due to final connection error");
                        return false;
                    }
                }
                (Err(err), phase) => {
                    tracing::warn!(%err, ?phase, "connection failed in unexpecting state");
                }
            },

            Results::DisconnectionResult { wg_public_key, res } => {
                match res {
                    Ok(_) => {
                        tracing::info!(%wg_public_key, "disconnected successful");
                    }
                    Err(err) => {
                        tracing::error!(?err, %wg_public_key, "disconnection failed");
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

            Results::TunnelPingResult { rtt } => {
                if let Phase::Connected(conn) = self.phase.clone()
                    && let Some(rh) = self.route_healths.get_mut(&conn.destination.id)
                {
                    let failures = rh.tunnel_ping_result(rtt);
                    let max = self.config.connection.health_check_intervals.tunnel_ping_max_failures;
                    if failures >= max {
                        tracing::warn!(%conn, failures, "tunnel ping exceeded max failures - reconnecting");
                        self.disconnect_from_connection(&conn, results_sender);
                    }
                }
            }

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

            Results::HealthCheck { id, outcome } => {
                tracing::info!(%id, ?outcome, "received health check");
                if let Some(dest) = self.config.destinations.get(&id).cloned()
                    && let Some(rh) = self.route_healths.get_mut(&id)
                {
                    let was_ready = rh.is_ready_to_connect();
                    rh.health_check_result(
                        outcome,
                        self.hopr.as_ref().unwrap(),
                        &dest,
                        &self.config.connection,
                        results_sender,
                    );
                    // Trigger connection if we just became ready
                    if !was_ready && rh.is_ready_to_connect() {
                        self.act_on_target(results_sender);
                    }
                }
            }
        };
        return true;
    }

    async fn on_results_safeless_interactor(
        &mut self,
        res: Result<SafelessInteractor, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match res {
            Ok(safeless_interactor) => {
                tracing::info!("safeless interactor created successfully");
                self.safeless_interactor = Some(Arc::new(safeless_interactor));
                self.spawn_ticket_stats_runner(results_sender, Duration::ZERO);
                self.determine_next_phase_from_safe_disk_query(results_sender).await;
            }
            Err(err) => {
                tracing::error!(?err, "failed to create safeless interactor - retrying in 30 seconds");
                self.spawn_initial_runner(results_sender, Duration::from_secs(30));
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
                Phase::CheckingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::info!(%presafe, "on presafe node balance");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::error!(?err, "failed to fetch presafe node balance - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Error(err.to_string()),
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring presafe node balance result in unexpected phase");
            }
        }
    }

    async fn on_results_query_safe(
        &mut self,
        res: Result<Option<SafeModule>, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (Ok(Some(safe_module)), Phase::CheckingSafe { .. }) => {
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
                Phase::CheckingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::info!("found no deployed safe module");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Success(None),
                    deploy_safe_error,
                    funding_tool,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::error!(?err, "failed to query safe module - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Error(err.to_string()),
                    deploy_safe_error,
                    funding_tool,
                };
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring query safe result in unexpected phase");
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
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error: Some(err.to_string()),
                    funding_tool: balance::FundingTool::NotStarted,
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring deploy safe result in unexpected phase");
            }
        }
    }

    fn on_results_funding_tool(&mut self, res: Result<Option<String>, runner::Error>) {
        match (res, self.phase.clone()) {
            (
                Ok(None),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedSuccess,
                }
            }
            (
                Ok(Some(reason)),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedError(reason),
                }
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedError(err.to_string()),
                }
            }

            (res, phase) => {
                tracing::warn!(?res, ?phase, "unexpected funding tool response in wrong phase");
            }
        }
    }

    fn trigger_deploy_safe(&mut self, results_sender: &mpsc::Sender<Results>) {
        if let Phase::CheckingSafe {
            node_balance: Querying::Success(presafe),
            query_safe: Querying::Success(None),
            deploy_safe_error: _,
            funding_tool: _,
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

    fn spawn_initial_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::create_safeless_interactor(&worker_params, blokli_config.into(), results_sender).await;
                })
                .await
        });
    }

    async fn determine_next_phase_from_safe_disk_query(&mut self, results_sender: &mpsc::Sender<Results>) {
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
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Init,
                    query_safe: Querying::Init,
                    deploy_safe_error: None,
                    funding_tool: balance::FundingTool::NotStarted,
                };
                self.spawn_query_safe_runner(results_sender, Duration::ZERO);
                self.spawn_node_balance_runner(results_sender, Duration::ZERO);
            }
        }
    }

    fn spawn_query_safe_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(safeless_interactor) = self.safeless_interactor.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::query_safe(safeless_interactor, results_sender).await
                    })
                    .await
            });
        }
    }

    fn spawn_node_balance_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(safeless_interactor) = self.safeless_interactor.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::node_balance(safeless_interactor, results_sender).await
                    })
                    .await
            });
        }
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
        let presafe = presafe.clone();
        let results_sender = results_sender.clone();
        if let Some(safeless_interactor) = self.safeless_interactor.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner::safe_deployment(safeless_interactor, presafe, results_sender).await;
                    })
                    .await
            });
        }
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
        let results_sender = results_sender.clone();
        if let Some(safeless_interactor) = self.safeless_interactor.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::ticket_stats(safeless_interactor, results_sender).await;
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
            tracing::debug!(address = %address.to_checksum(), "channel funding already ongoing - skipping");
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

    fn spawn_connection_runner(
        &mut self,
        destination: Destination,
        exit: route_health::ExitHealth,
        results_sender: &mpsc::Sender<Results>,
    ) {
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
            if let Some(rh) = self.route_healths.get_mut(&destination.id) {
                rh.connecting(
                    self.hopr.as_ref().unwrap(),
                    &destination,
                    exit,
                    &self.config.connection,
                    &results_sender,
                );
            }
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

    fn spawn_tunnel_ping_probe(&self, results_sender: &mpsc::Sender<Results>) {
        let interval = self.config.connection.health_check_intervals.tunnel_ping;
        let cancel = self.cancel_connection.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::tunnel_ping_loop(interval, results_sender).await;
                })
                .await
        });
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    fn act_on_target(&mut self, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(target = ?self.target_destination, phase = ?self.phase, "acting on target destination");
        match (self.target_destination.clone(), self.phase.clone()) {
            // Connecting from ready
            (Some(dest), Phase::HoprRunning) => {
                if let Some(rh) = self.route_healths.get(&dest.id) {
                    if let Some(exit) = rh.ready_to_connect() {
                        tracing::info!(destination = %dest, "establishing connection to new destination");
                        self.spawn_connection_runner(dest.clone(), exit, results_sender);
                    } else if rh.is_unrecoverable() {
                        tracing::error!(destination = %dest,route_health = ?rh.state(),  "refusing connection because of route health");
                    } else {
                        tracing::warn!(destination = %dest,route_health = ?rh.state(),  "waiting for better route health before connecting");
                    }
                } else {
                    tracing::warn!(destination = %dest, "refusing connection: destination has no route health tracker");
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
        if let Some(dest) = self.config.destinations.get(&conn.destination.id).cloned()
            && let Some(rh) = self.route_healths.get_mut(&conn.destination.id)
        {
            rh.disconnecting(
                self.hopr.as_ref().unwrap(),
                &dest,
                &self.config.connection,
                results_sender,
            );
        }
        if let Ok(disconn) = conn.try_into() {
            self.spawn_disconnection_runner(&disconn, results_sender);
        } else {
            // connection did not even generate a wg pub key - so we can immediately try to connect again
            self.act_on_target(results_sender);
        }
    }

    fn on_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        if route_health::any_needs_peers(self.route_healths.values()) {
            self.spawn_connected_peers(results_sender, Duration::ZERO);
        } else {
            self.act_on_target(results_sender);
        }
    }

    fn try_start_reactor(&mut self, results_sender: &mpsc::Sender<Results>) {
        if self.strategy_handle.is_some() {
            return;
        }
        let Some(edgli) = self.hopr.as_ref() else { return };
        let Some(tv) = self.ticket_value else { return };
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

    fn on_ticket_stats(&mut self, stats: TicketStats, results_sender: &mpsc::Sender<Results>) {
        tracing::info!("received ticket stats from runner");
        match stats.ticket_value() {
            Ok(tv) => {
                tracing::info!(%stats, %tv, "determined ticket value from stats");
                self.ticket_value = Some(tv);
                self.try_start_reactor(results_sender);
            }
            Err(err) => {
                tracing::error!(?err, %stats, "failed to determine ticket value from stats - retrying");
                self.spawn_ticket_stats_runner(results_sender, Duration::from_secs(10));
            }
        }
    }
}
