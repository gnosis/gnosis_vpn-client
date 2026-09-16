use edgli::EdgliInitState;
use edgli::blokli::IncentiveOperations;
use edgli::hopr_lib::builder::Keypair;
use futures_util::future::AbortHandle;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::collections::{HashMap, HashSet};
use std::net;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::command::{self, Response, RunMode, WorkerCommand};
use crate::compat::SafeModule;
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::{self, Address, Destination, Destinations, ExitKey, HopRouting};
use crate::event::{CoreToWorker, RequestToRoot, ResponseFromRoot, RunnerToRoot, WorkerToCore};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, config as hopr_config, identity};
use crate::probe::{self, Probe};
use crate::route_health::RouteHealth;
use crate::worker_params::{self, WorkerParams};
use crate::{balance, log_output, ticket_stats, wireguard};

pub(crate) mod runner;

use runner::Results;

enum Responder {
    Unit(oneshot::Sender<Result<(), String>>),
    Str(oneshot::Sender<Result<String, String>>),
    Duration(oneshot::Sender<Result<Duration, String>>),
}

const NODE_WXHOPR_WITHDRAW_INTERVAL: Duration = Duration::from_secs(45);
/// Graph walk cadence while a destination is not routable yet or a target is pending.
const ROUTABILITY_EAGER_INTERVAL: Duration = Duration::from_secs(10);
/// Graph walk cadence once every destination has settled.
const ROUTABILITY_LAZY_INTERVAL: Duration = Duration::from_secs(60);

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
    #[error("IncentiveOperations creation error: {0}")]
    IncentiveOperationsCreation(String),
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
    cancel_connection: CancellationToken,
    cancel_node_wxhopr: CancellationToken,
    cancel_on_shutdown: CancellationToken,
    cancel_presafe_queries: CancellationToken,
    cancel_balances: CancellationToken,
    // Tracks the connection's NepTUN pump task so teardown can wait for it to
    // stop (dropping its TUN fd) before asking root to tear down routing;
    // replaced together with cancel_connection.
    wg_pump_tasks: TaskTracker,

    /// One graph walk in flight at a time; replaced on every spawn.
    cancel_routability: CancellationToken,
    announced_peers_loop_running: bool,

    // user provided data
    /// The connect id, resolved live - a cached clone goes stale on a discovery tick and cannot outlive a restart.
    target: Option<String>,

    // runtime data
    phase: Phase,
    funding_tool: balance::FundingTool,
    incentive_operations: Option<Arc<dyn IncentiveOperations>>,
    hopr: Option<Arc<Hopr>>,
    minimum_balance_recommendation: Option<balance::BalanceRecommendation>,
    ideal_balance_recommendation: Option<balance::BalanceRecommendation>,
    capacity_allocations: Option<balance::CapacityAllocations>,
    capacity_reconciler: balance::CapacityReconciler,
    balances: Option<balance::Balances>,
    strategy_handle: Option<AbortHandle>,
    route_healths: HashMap<ExitKey, RouteHealth>,
    /// The one long-lived probe session; connections register over it.
    probe: Option<Probe>,
    probe_generation: u64,
    /// A previous connection's key a force-reconnect could not unregister yet, for the next connection runner.
    stale_wg_public_key: Option<String>,
    next_request_id: u64,
    // Maps a request_id to the oneshot sender waiting for root's response.
    // request_id is needed even though at most one request is in-flight at a time:
    // root runs pings in a JoinSet, so a stale ping from a cancelled connection
    // can arrive after a new responder has been stored — the id mismatch discards it.
    // Cleared in disconnect_from_connection so stale entries don't outlive their connection.
    responders: HashMap<u64, Responder>,
    ongoing_disconnections: Vec<connection::down::Down>,
    cached_resolved_blokli_ips: Vec<net::Ipv4Addr>,
    reconnecting_since: Option<SystemTime>,
}

#[derive(Debug, Clone)]
enum Phase {
    // initial phase — create the IncentiveOperations handle (Blokli-backed)
    // and determine if a Safe has been deployed for this node
    Initial {
        last_error: Option<String>,
    },
    /// safe absent or safe deployment error - repeatedly query node balance and safe info
    CheckingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
        deploy_safe_error: Option<String>,
    },
    /// enough funds and no deployed safe - run safe deployment
    DeployingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
    },
    /// construct edge client
    Starting {
        edgli_init_state: Option<EdgliInitState>,
        last_error: Option<String>,
    },
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
        let keys = worker_params.persist_identity_generation().await?;
        let node_address = keys.chain_key.public().to_address();
        let cancel_on_shutdown = CancellationToken::new();
        let mut route_healths = HashMap::new();
        for (key, dest) in config.destinations.iter() {
            route_healths.insert(
                *key,
                RouteHealth::new(dest, worker_params.allow_insecure(), worker_params.allow_experimental()),
            );
        }

        // Root replays the connect id; one discovery has not published yet just waits for it.
        let target = target_dest_id;

        let (incoming_sender, incoming_receiver) = mpsc::channel(32);
        let cached_resolved_blokli_ips = worker_params.cached_blokli_ips().to_vec();
        let core = Core {
            // config data
            config,

            // static data
            worker_params,
            node_address,
            outgoing_sender,
            incoming_receiver,

            // cancellation tokens
            cancel_connection: cancel_on_shutdown.child_token(),
            cancel_node_wxhopr: cancel_on_shutdown.child_token(),
            cancel_on_shutdown: cancel_on_shutdown.clone(),
            cancel_presafe_queries: cancel_on_shutdown.child_token(),
            cancel_balances: cancel_on_shutdown.child_token(),
            wg_pump_tasks: TaskTracker::new(),
            cancel_routability: cancel_on_shutdown.child_token(),
            announced_peers_loop_running: false,

            // user provided data
            target,

            // runtime data
            phase: Phase::Initial { last_error: None },
            funding_tool: balance::FundingTool::NotStarted,
            hopr: None,
            incentive_operations: None,
            minimum_balance_recommendation: None,
            ideal_balance_recommendation: None,
            capacity_allocations: None,
            capacity_reconciler: balance::CapacityReconciler::default(),
            balances: None,
            strategy_handle: None,
            ongoing_disconnections: Vec::new(),
            route_healths,
            probe: None,
            probe_generation: 0,
            stale_wg_public_key: None,
            next_request_id: 0,
            responders: HashMap::new(),
            // needed to keep working during enabled killswitch
            cached_resolved_blokli_ips,
            reconnecting_since: None,
        };
        Ok((core, incoming_sender))
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
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
    #[tracing::instrument(skip(self, results_sender), level = "debug")]
    async fn on_event(&mut self, event: WorkerToCore, results_sender: &mpsc::Sender<Results>) -> bool {
        match event {
            WorkerToCore::Shutdown => {
                tracing::debug!("incoming shutdown request");
                self.phase = Phase::ShuttingDown;
                // no need to recreate cancellation tokens after shutdown
                self.cancel_on_shutdown.cancel();
                // Dropping the probe detaches its session close before hopr aborts the listeners.
                self.probe.take();
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
                    ResponseFromRoot::KillswitchLockdown { request_id, res } => {
                        if let Some(Responder::Unit(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for killswitch lockdown response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for killswitch lockdown response (evicted or duplicate)"
                            );
                        }
                    }
                    ResponseFromRoot::TunnelReady { request_id, res } => {
                        if let Some(Responder::Str(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for tunnel ready response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for tunnel ready response (evicted or duplicate)"
                            );
                        }
                    }
                    ResponseFromRoot::Ping { request_id, res } => {
                        if let Some(Responder::Duration(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for ping response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for ping response (evicted or duplicate)"
                            );
                        }
                    }
                };

                true
            }

            WorkerToCore::WorkerCommand { cmd, resp } => {
                // Status is polled frequently; keep it at trace to avoid log spam.
                if matches!(&cmd, WorkerCommand::Status) {
                    tracing::trace!(%cmd, "incoming command");
                } else {
                    tracing::debug!(%cmd, "incoming command");
                }
                match cmd {
                    WorkerCommand::NerdStats => {
                        tracing::debug!("incoming nerd stats request");
                        let Some(ops) = self.incentive_operations.clone() else {
                            let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse {
                                connection: command::NerdStatsConnection::NoInfo(command::TicketStatsStatus::Waiting),
                            }));
                            return true;
                        };
                        let sender = results_sender.clone();
                        tokio::spawn(async move {
                            let ticket_stats_status = match ops.ticket_stats().await {
                                Ok(ts) => command::TicketStatsStatus::Available(ticket_stats::TicketStats {
                                    ticket_price: ts.ticket_price,
                                    winning_probability: ts.winning_probability.into(),
                                }),
                                Err(e) => command::TicketStatsStatus::Error(e.to_string()),
                            };
                            let _ = sender
                                .send(Results::NerdStatsTicketStats {
                                    res: ticket_stats_status,
                                    resp,
                                })
                                .await;
                        });
                    }

                    WorkerCommand::Status => {
                        let _ = resp.send(Response::status(self.build_status()));
                    }

                    WorkerCommand::Destinations => {
                        let _ = resp.send(Response::Destinations(self.config.destinations.connect_ids()));
                    }

                    WorkerCommand::Connect(token) => match self.config.destinations.resolve(&token).cloned() {
                        Ok(dest) => {
                            self.reconnecting_since = None;
                            let is_already_active = match &self.phase {
                                Phase::Connected(conn) | Phase::Connecting(conn) => conn.destination.same_exit(&dest),
                                _ => false,
                            };
                            if is_already_active {
                                let _ = resp.send(Response::connect(command::ConnectResponse::already_connected(
                                    dest.clone(),
                                )));
                            } else if let Some(rh) = self.route_healths.get(&dest.key()) {
                                if rh.is_unrecoverable() {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::unable(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                } else if rh.is_routable() {
                                    // Opening the probe is part of connecting; act_on_target sequences it.
                                    let _ = resp
                                        .send(Response::connect(command::ConnectResponse::connecting(dest.clone())));
                                    self.target = Some(dest.connect_id.clone());
                                    self.act_on_target(results_sender);
                                } else {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::waiting(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                    self.target = Some(dest.connect_id.clone());
                                }
                            } else {
                                tracing::warn!(key = %dest.key(), "no route health found for destination - this should not happen");
                                let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                            }
                        }
                        Err(destination::Unresolved::Ambiguous(ids)) => {
                            tracing::info!(%token, ?ids, "connect names one exit reached by several paths");
                            let _ = resp.send(Response::connect(command::ConnectResponse::ambiguous(ids)));
                        }
                        Err(destination::Unresolved::NotFound) => {
                            tracing::info!(%token, "cannot connect to destination - not known");
                            let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                        }
                    },

                    WorkerCommand::Disconnect => {
                        self.target = None;
                        self.reconnecting_since = None;
                        self.cached_resolved_blokli_ips = Vec::new();
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

                    WorkerCommand::Probe(token) => {
                        let response = match self.config.destinations.resolve(&token).cloned() {
                            Ok(dest) => self.probe_destination(dest, results_sender),
                            Err(destination::Unresolved::Ambiguous(ids)) => {
                                command::ProbeResponse::DestinationAmbiguous { connect_ids: ids }
                            }
                            Err(destination::Unresolved::NotFound) => command::ProbeResponse::DestinationNotFound,
                        };
                        let _ = resp.send(Response::Probe(response));
                    }

                    WorkerCommand::Unprobe => {
                        let response = match self.probe.as_ref() {
                            None => command::UnprobeResponse::NotProbing,
                            Some(p) => {
                                // A connection attempt registers over this session; let it finish first.
                                let registering =
                                    matches!(&self.phase, Phase::Connecting(conn) if conn.destination.key() == p.key());
                                if registering {
                                    command::UnprobeResponse::in_use(p.destination().clone())
                                } else {
                                    let destination = p.destination().clone();
                                    self.stop_probe();
                                    command::UnprobeResponse::closing(destination)
                                }
                            }
                        };
                        let _ = resp.send(Response::Unprobe(response));
                    }

                    WorkerCommand::Balance => {
                        let result = match (&self.hopr, &self.balances) {
                            (Some(hopr), Some(balances)) => {
                                let funding_status =
                                    match (&self.ideal_balance_recommendation, &self.capacity_allocations) {
                                        (Some(ideal), Some(allocs)) => {
                                            Some(balance::to_funding_status(*ideal, allocs, balances.node_xdai))
                                        }
                                        _ => None,
                                    };
                                Ok(command::BalanceResponse::build(
                                    &hopr.info(),
                                    balances,
                                    &self.config.destinations,
                                    self.capacity_allocations.as_ref(),
                                    self.ideal_balance_recommendation,
                                    funding_status,
                                ))
                            }
                            _ => Err("balance data not yet available".to_string()),
                        };
                        let _ = resp.send(Response::Balance(result));
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

                    WorkerCommand::ForceReconnect => {
                        let active_conn = match self.phase.clone() {
                            Phase::Connected(conn) => Some(conn),
                            Phase::Connecting(conn) => Some(conn),
                            _ => None,
                        };
                        if let Some(conn) = active_conn {
                            tracing::info!(%conn, "force reconnect triggered by WAN change");
                            self.reconnecting_since = Some(SystemTime::now());
                            self.force_reconnect(conn, results_sender).await;
                        } else {
                            tracing::debug!(?self.phase, "force reconnect requested but not connected or connecting");
                        }
                        let _ = resp.send(Response::ForceReconnectAcknowledged);
                    }

                    WorkerCommand::FundingTool(secret) => {
                        let in_presafe_phase = matches!(self.phase, Phase::CheckingSafe { .. });
                        let rerun_allowed = self.worker_params.allow_funding_tool_rerun();
                        // cooldown only gates reruns; without the flag, a completed run stays Done forever
                        let cooldown_remaining =
                            rerun_allowed.then(|| self.funding_tool.cooldown_remaining()).flatten();

                        let response = if !(in_presafe_phase || rerun_allowed) {
                            command::FundingToolResponse::WrongPhase
                        } else if let Some(remaining) = cooldown_remaining {
                            command::FundingToolResponse::Cooldown(remaining)
                        } else {
                            match &self.funding_tool {
                                balance::FundingTool::InProgress => command::FundingToolResponse::InProgress,
                                balance::FundingTool::CompletedSuccess(_) if !rerun_allowed => {
                                    command::FundingToolResponse::Done
                                }
                                balance::FundingTool::NotStarted
                                | balance::FundingTool::CompletedError(_)
                                | balance::FundingTool::CompletedSuccess(_) => {
                                    self.funding_tool = balance::FundingTool::InProgress;
                                    self.spawn_funding_runner(secret, results_sender);
                                    command::FundingToolResponse::Started
                                }
                            }
                        };
                        let _ = resp.send(Response::funding_tool(response));
                    }
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
            Results::IncentiveOperations { res } => {
                if !self.on_results_incentive_operations(res, results_sender).await {
                    return false;
                }
            }
            Results::IncentiveOperationsRetry { error } => {
                if matches!(self.phase, Phase::Initial { .. }) {
                    self.phase = Phase::Initial {
                        last_error: Some(error),
                    };
                }
            }
            Results::ExitNodesUpdated { nodes } => {
                self.merge_discovered_destinations(nodes, results_sender);
            }
            Results::ExitNodesRetry { error } => {
                tracing::warn!(%error, "exit node discovery failed - retrying");
                self.spawn_exit_node_discovery_runner(results_sender, Duration::from_secs(30));
            }
            Results::HoprConstruction(edgli_state) => {
                if matches!(self.phase, Phase::Starting { .. }) {
                    self.phase = Phase::Starting {
                        edgli_init_state: Some(edgli_state),
                        last_error: None,
                    };
                } else {
                    tracing::warn!(?self.phase, "hopr construction result received in unexpected phase");
                }
            }
            Results::MinimumBalanceRecommendation { res } => match res {
                Ok(rec) => {
                    tracing::info!(?rec, "received minimum balance recommendation");
                    self.minimum_balance_recommendation = Some(rec);
                    // safe deployment may be waiting on this recommendation
                    self.trigger_deploy_safe(results_sender);
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch minimum balance recommendation - retrying");
                    self.spawn_minimum_balance_recommendation_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::IdealBalanceRecommendation { res } => match res {
                Ok(rec) => {
                    tracing::info!(?rec, "received ideal balance recommendation");
                    self.ideal_balance_recommendation = Some(rec);
                    self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to fetch ideal balance recommendation - retrying");
                    self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::CapacityAllocations { res } => match res {
                Ok(caps) => {
                    let caps = self.capacity_reconciler.reconcile(caps);
                    tracing::info!(%caps, "received capacity allocations");
                    self.capacity_allocations = Some(caps);
                    self.spawn_capacity_allocations_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to fetch capacity allocations - retrying");
                    self.spawn_capacity_allocations_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::Balances { res } => match res {
                Ok(balances) => {
                    tracing::info!(%balances, "received balances from hopr");
                    self.balances = Some(balances);
                    self.spawn_balances_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch balances from hopr - retrying");
                    self.spawn_balances_runner(results_sender, Duration::from_secs(10));
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
                    self.spawn_node_wxhopr_withdraw_runner(results_sender, Duration::ZERO);
                    self.try_start_reactor(results_sender).await;
                    self.spawn_wait_for_running(results_sender, Duration::from_secs(1));
                }
                Err(err) => {
                    tracing::error!(?err, "hopr runner failed to start - trying again in 10 seconds");
                    self.retry_hopr_runner(err.to_string(), safe_module, results_sender, Duration::from_secs(10));
                }
            },

            // The runner retries indefinitely so res is always Ok; log just in case.
            Results::NodeWxhoprWithdraw { res } => {
                if let Err(err) = res {
                    tracing::error!(?err, "failed to withdraw node wxHOPR to safe");
                }
                self.spawn_node_wxhopr_withdraw_runner(results_sender, NODE_WXHOPR_WITHDRAW_INTERVAL);
            }

            Results::HoprRunning => {
                self.on_hopr_running(results_sender);
            }

            Results::AnnouncedPeers { res } => match res {
                Ok(announced) => {
                    tracing::info!(num_announced = %announced.len(), "fetched announced peers");
                    let peer_ips: Vec<net::Ipv4Addr> =
                        announced.values().flat_map(|p| p.ipv4_addrs.iter().copied()).collect();
                    let _ = self
                        .outgoing_sender
                        .send(CoreToWorker::RequestToRoot(RequestToRoot::UpdatePeerIps { peer_ips }))
                        .await;
                }
                Err(err) => tracing::error!(?err, "failed to fetch announced peers"),
            },

            Results::Routability { map } => {
                let mut became_routable = false;
                for (key, routable) in map {
                    if let Some(rh) = self.route_healths.get_mut(&key) {
                        became_routable |= rh.set_routable(routable);
                    }
                }
                if became_routable && self.target.is_some() {
                    self.act_on_target(results_sender);
                }
                let settling = self
                    .route_healths
                    .values()
                    .any(|rh| !rh.is_routable() && !rh.is_unrecoverable());
                let target_pending = self.target.is_some() && !matches!(self.phase, Phase::Connected(_));
                let delay = if settling || target_pending {
                    ROUTABILITY_EAGER_INTERVAL
                } else {
                    ROUTABILITY_LAZY_INTERVAL
                };
                self.spawn_routability_runner(results_sender, delay);
            }

            Results::ConnectionEvent(evt) => {
                tracing::debug!(%evt, "handling connection runner event");
                match self.phase.clone() {
                    Phase::Connecting(mut conn) => match evt {
                        connection::up::Event::Progress(e) => {
                            if let connection::up::Progress::GenerateWg(blokli_ips) = e.as_ref() {
                                self.cached_resolved_blokli_ips = blokli_ips.clone();
                                let request = RequestToRoot::CacheBlokliIps {
                                    ips: blokli_ips.clone(),
                                };
                                let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                            }
                            conn.connect_progress(*e);
                            self.phase = Phase::Connecting(conn);
                        }
                        connection::up::Event::Setback(e) => {
                            if let Some(rh) = self.route_healths.get_mut(&conn.destination.key()) {
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
                // Not earlier: a probe swap would close the session the down runner still unregisters over.
                if matches!(evt, connection::down::Event::CloseBridge) {
                    self.act_on_target(results_sender);
                }
            }

            Results::ConnectionResult { res } => match (res, self.phase.clone()) {
                (Ok(_session), Phase::Connecting(mut conn)) => {
                    tracing::info!(%conn, "connection established successfully");
                    self.reconnecting_since = None;
                    conn.connected();
                    self.phase = Phase::Connected(conn.clone());
                    let route = format!(
                        "{}({})",
                        conn.destination.pretty_print_path(),
                        log_output::address(&conn.destination.address)
                    );
                    log_output::print_session_established(route.as_str());
                    // A spliced session has no local listener to poll; the pump task
                    // reports its own death via WgPumpExited instead of a monitor.
                    self.spawn_tunnel_ping_probe(results_sender);
                    if conn.surb_target.is_some() {
                        self.spawn_surb_ramp_ticker(results_sender);
                    }
                }
                (Ok(_), phase) => {
                    tracing::warn!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(?err, %conn, "connection failed");
                    self.reconnecting_since = None;
                    if let Some(rh) = self.route_healths.get_mut(&conn.destination.key()) {
                        rh.with_error(err.to_string());
                    }
                    // By identity: a dropped target must still restart the worker, not stick in Connecting.
                    let failed_the_target = self.target.as_deref() == Some(conn.destination.connect_id.as_str());
                    if failed_the_target {
                        tracing::info!(id = %conn.destination.connect_id, "restarting connection worker process due to final connection error");
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

            Results::WgPumpExited { reason } => match self.phase.clone() {
                Phase::Connected(conn) => {
                    tracing::warn!(%conn, %reason, "wg pump exited - reconnecting");
                    self.reconnecting_since = Some(SystemTime::now());
                    self.disconnect_from_connection(&conn, results_sender);
                }
                phase => {
                    // During Connecting the runner's own tunnel ping verification
                    // surfaces the failure; in any other phase the connection is
                    // already being torn down.
                    tracing::debug!(?phase, %reason, "wg pump exited outside an established connection");
                }
            },

            Results::TunnelPingResult { rtt } => {
                if let Phase::Connected(mut conn) = self.phase.clone() {
                    let failures = conn.tunnel_ping_result(rtt);
                    self.phase = Phase::Connected(conn.clone());
                    let max = self.config.connection.health_check_intervals.tunnel_ping_max_failures;
                    if failures >= max {
                        tracing::warn!(%conn, failures, "tunnel ping exceeded max failures - reconnecting");
                        self.reconnecting_since = Some(SystemTime::now());
                        self.disconnect_from_connection(&conn, results_sender);
                    }
                }
            }

            Results::WgStatsSample(sample) => match self.phase.clone() {
                Phase::Connecting(mut conn) => {
                    conn.record_wg_stats(sample);
                    self.phase = Phase::Connecting(conn);
                }
                Phase::Connected(mut conn) => {
                    conn.record_wg_stats(sample);
                    self.phase = Phase::Connected(conn);
                }
                phase => {
                    tracing::debug!(?phase, "received wg stats sample outside an active connection");
                }
            },

            Results::SurbRampTick => match self.phase.clone() {
                Phase::Connected(mut conn) => {
                    if let Some(configurator) = conn.session_configurator.clone() {
                        conn.advance_surb_ramp(&configurator, SystemTime::now());
                    }
                    self.phase = Phase::Connected(conn);
                }
                phase => {
                    tracing::warn!(?phase, "received surb ramp tick outside a connected phase");
                }
            },

            Results::ConnectionRequestToRoot(respondable_request) => match respondable_request {
                RunnerToRoot::KillswitchLockdown {
                    peer_ips,
                    interface,
                    resp,
                } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Unit(resp));
                    let request = RequestToRoot::KillswitchLockdown {
                        request_id,
                        peer_ips,
                        interface,
                    };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::SetupTunnel {
                    interface_address,
                    mtu,
                    dns,
                    peer_ips,
                    resp,
                } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Str(resp));
                    let request = RequestToRoot::SetupTunnel {
                        request_id,
                        interface_address,
                        mtu,
                        dns,
                        peer_ips,
                    };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::Ping { options, resp } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Duration(resp));
                    let request = RequestToRoot::Ping { request_id, options };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }
            },

            Results::Probe { generation, event } => {
                let Some(probe) = self.probe.as_mut().filter(|p| p.generation() == generation) else {
                    tracing::debug!(generation, ?event, "dropping event of a replaced probe");
                    return true;
                };
                let key = probe.key();
                let was_ready = probe.ready_session().is_some();
                let reopening = matches!(event, probe::Event::Reopening { .. });
                if let probe::Event::Version { versions, .. } = &event
                    && let Some(rh) = self.route_healths.get_mut(&key)
                {
                    match probe::select_api_version(&versions.versions) {
                        Some(_) => rh.clear_incompatible(),
                        None => rh.set_incompatible(versions.versions.clone()),
                    }
                }
                probe.apply(event);
                let is_ready = probe.ready_session().is_some();
                if !was_ready && is_ready {
                    self.act_on_target(results_sender);
                }
                // A connection still registering holds a bound_host that just died; restart the attempt.
                if reopening
                    && let Phase::Connecting(conn) = self.phase.clone()
                    && conn.destination.key() == key
                    && conn.registration.is_none()
                {
                    tracing::warn!(%conn, "probe session broke during registration - restarting the attempt");
                    self.disconnect_from_connection(&conn, results_sender);
                }
            }

            Results::RetryReactor => {
                self.try_start_reactor(results_sender).await;
            }

            Results::NerdStatsTicketStats {
                res: ticket_stats_status,
                resp,
            } => match &self.phase {
                Phase::Connecting(conn) => {
                    let bridge = self.probe_session_for(conn);
                    let telemetry = nerd_stats_telemetry(conn, bridge);
                    let conn_stats =
                        command::ConnStats::from_conn(conn, self.node_address, telemetry.as_deref(), bridge);
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse {
                        connection: command::NerdStatsConnection::Connecting(ticket_stats_status, conn_stats),
                    }));
                }
                Phase::Connected(conn) => {
                    let bridge = self.probe_session_for(conn);
                    let telemetry = nerd_stats_telemetry(conn, bridge);
                    let conn_stats =
                        command::ConnStats::from_conn(conn, self.node_address, telemetry.as_deref(), bridge);
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse {
                        connection: command::NerdStatsConnection::Connected(ticket_stats_status, conn_stats),
                    }));
                }
                _ => {
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse {
                        connection: command::NerdStatsConnection::NoInfo(ticket_stats_status),
                    }));
                }
            },
        };
        return true;
    }

    // Returns false to signal core exit, which lets root restart the worker.
    async fn on_results_incentive_operations(
        &mut self,
        res: Result<Arc<dyn IncentiveOperations>, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) -> bool {
        match res {
            Ok(incentive_operations) => {
                tracing::info!("incentive operations handle created successfully");
                self.incentive_operations = Some(incentive_operations);
                self.spawn_minimum_balance_recommendation_runner(results_sender, Duration::ZERO);
                self.determine_next_phase_from_safe_disk_query(results_sender).await;
                true
            }
            Err(err) => {
                tracing::error!(
                    ?err,
                    "failed to create incentive operations handle after all retries - restarting core"
                );
                false
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
                },
            ) => {
                tracing::info!(%presafe, "on presafe node balance");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe,
                    deploy_safe_error,
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
                },
            ) => {
                tracing::error!(?err, "failed to fetch presafe node balance - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Error(err.to_string()),
                    query_safe,
                    deploy_safe_error,
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
                self.cancel_presafe_queries = self.cancel_on_shutdown.child_token();
                // start edge client with queried safe module
                self.start_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
                // try persisting safe module to disk - might fail but we consider this non critical
                self.spawn_store_safe(safe_module, results_sender, Duration::ZERO);
            }
            (
                Ok(None),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe_error,
                },
            ) => {
                tracing::info!("found no deployed safe module");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Success(None),
                    deploy_safe_error,
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
                },
            ) => {
                tracing::error!(?err, "failed to query safe module - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Error(err.to_string()),
                    deploy_safe_error,
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
                self.start_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
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
        self.funding_tool = match res {
            Ok(None) => balance::FundingTool::CompletedSuccess(SystemTime::now()),
            Ok(Some(reason)) => balance::FundingTool::CompletedError(reason),
            Err(err) => balance::FundingTool::CompletedError(err.to_string()),
        };
    }

    fn trigger_deploy_safe(&mut self, results_sender: &mpsc::Sender<Results>) {
        if let Phase::CheckingSafe {
            node_balance: Querying::Success(presafe),
            query_safe: Querying::Success(None),
            deploy_safe_error: _,
        } = self.phase.clone()
        {
            let Some(recommendation) = self.minimum_balance_recommendation else {
                tracing::info!(balance = %presafe, "waiting for minimum balance recommendation before safe deployment");
                return;
            };
            if presafe.node_wxhopr < recommendation.wxhopr || presafe.node_xdai < recommendation.xdai {
                tracing::warn!(
                    balance = %presafe,
                    required_wxhopr = %recommendation.wxhopr,
                    required_xdai = %recommendation.xdai,
                    "insufficient funds to start safe deployment - waiting for funding"
                );
            } else {
                self.phase = Phase::DeployingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe: Querying::Success(None),
                };
                self.cancel_presafe_queries.cancel();
                self.cancel_presafe_queries = self.cancel_on_shutdown.child_token();
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
                    runner::create_incentive_operations(&worker_params, blokli_config, results_sender).await;
                })
                .await
        });
    }

    /// Only runs once the node is up, since tracking the registry needs its chain connector — so a
    /// config with no `[destinations]` has none until then. The guard also stops the
    /// `ExitNodesRetry` re-spawn from resurrecting discovery outside that window.
    fn spawn_exit_node_discovery_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let Some(hopr) = self.hopr.clone() else { return };
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::watch_exit_nodes(&worker_params, blokli_config, hopr, results_sender).await;
                })
                .await
        });
    }

    /// Merges a discovery snapshot in and keeps `route_healths` in step, mirroring `Core::init`.
    ///
    /// Leaves the state machine alone: a live connection holds its own `Destination` regardless.
    fn merge_discovered_destinations(
        &mut self,
        nodes: HashMap<Address, edgli::ExitNodeInfo>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        let targets_before: HashMap<ExitKey, (net::SocketAddr, net::SocketAddr)> = self
            .config
            .destinations
            .iter()
            .map(|(key, dest)| (*key, (dest.gnosis_vpn_server, dest.wireguard_server)))
            .collect();
        // Trackers are what is synced, so one kept past its exit is still seen by the next merge.
        let before: HashSet<ExitKey> = self.route_healths.keys().copied().collect();
        let defaults = self.config.default_targets;
        self.config.destinations.merge_discovered(&nodes, defaults);
        let after: HashSet<ExitKey> = self.config.destinations.keys().copied().collect();

        let active = match &self.phase {
            Phase::Connected(conn) | Phase::Connecting(conn) => Some(conn.destination.key()),
            _ => None,
        };
        for removed in trackers_to_drop(&before, &after, active) {
            self.route_healths.remove(&removed);
        }

        let mut fresh: Vec<ExitKey> = after.difference(&before).copied().collect();
        fresh.extend(latched_on_a_moved_target(
            &self.config.destinations,
            &self.route_healths,
            &targets_before,
        ));
        for key in &fresh {
            if let Some(dest) = self.config.destinations.get(key) {
                self.route_healths.insert(
                    *key,
                    RouteHealth::new(
                        dest,
                        self.worker_params.allow_insecure(),
                        self.worker_params.allow_experimental(),
                    ),
                );
            }
        }

        let probe_vanished = self
            .probe
            .as_ref()
            .is_some_and(|p| !self.config.destinations.contains_key(&p.key()) && Some(p.key()) != active);
        if probe_vanished {
            tracing::info!("probed exit vanished from discovery - closing its session");
            self.stop_probe();
        }

        // A tracker that just started over needs a graph walk now, not on the lazy tick.
        if !fresh.is_empty() {
            self.spawn_routability_runner(results_sender, Duration::ZERO);
        }
    }

    async fn determine_next_phase_from_safe_disk_query(&mut self, results_sender: &mpsc::Sender<Results>) {
        let res = hopr_config::read_safe(self.worker_params.state_home()).await;
        match res {
            Ok(safe_module) => {
                tracing::debug!(?safe_module, "found existing safe module - starting hopr runner");
                // start edge client with existing safe module
                self.start_hopr_runner(safe_module, results_sender, Duration::ZERO);
            }
            Err(err) => {
                if matches!(err, hopr_config::Error::NoFile) {
                    tracing::info!("no persisted safe module found - querying incentive operations");
                } else {
                    tracing::warn!(
                        ?err,
                        "error deserializing existing safe module - querying incentive operations"
                    );
                }
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Init,
                    query_safe: Querying::Init,
                    deploy_safe_error: None,
                };
                self.spawn_query_safe_runner(results_sender, Duration::ZERO);
                self.spawn_node_balance_runner(results_sender, Duration::ZERO);
            }
        }
    }

    fn spawn_query_safe_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::query_safe(incentive_operations, results_sender).await
                    })
                    .await
            });
        }
    }

    fn spawn_node_balance_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::node_balance(incentive_operations, results_sender).await
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
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner::safe_deployment(incentive_operations, presafe, results_sender).await;
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
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let path_planner_min_ack_rate = self.config.connection.path_planner_min_ack_rate;
        let path_planner = self.config.connection.path_planner.clone();
        let probe_local_addresses = self.config.connection.probe_local_addresses;
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::hopr(
                        worker_params,
                        blokli_config,
                        path_planner_min_ack_rate,
                        path_planner,
                        probe_local_addresses,
                        &safe_module,
                        results_sender,
                    )
                    .await;
                })
                .await
        });
    }

    fn start_hopr_runner(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        self.phase = Phase::Starting {
            edgli_init_state: None,
            last_error: None,
        };
        self.spawn_hopr_runner(safe_module, results_sender, delay);
    }

    fn retry_hopr_runner(
        &mut self,
        error: String,
        safe_module: SafeModule,
        results_sender: &mpsc::Sender<Results>,
        delay: Duration,
    ) {
        self.phase = Phase::Starting {
            edgli_init_state: None,
            last_error: Some(error),
        };
        self.spawn_hopr_runner(safe_module, results_sender, delay);
    }

    fn spawn_minimum_balance_recommendation_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        let cfg = self.config.strategy.clone().into();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::minimum_balance_recommendation(incentive_operations, cfg, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_ideal_balance_recommendation_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let cfg = self.config.strategy.clone().into();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::ideal_balance_recommendation(hopr, cfg, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_capacity_allocations_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::capacity_allocations(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_balances_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
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

    fn spawn_node_wxhopr_withdraw_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let (Some(ops), Some(hopr)) = (self.incentive_operations.clone(), self.hopr.clone()) {
            let safe_address = hopr.info().safe_address;
            let cancel = self.cancel_node_wxhopr.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::node_wxhopr_withdraw(ops, safe_address, results_sender).await;
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

    fn start_announced_peers_loop(&mut self, results_sender: &mpsc::Sender<Results>) {
        if self.announced_peers_loop_running {
            return;
        }
        let Some(hopr) = self.hopr.clone() else { return };
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        self.announced_peers_loop_running = true;
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(runner::announced_peers_loop(hopr, results_sender))
                .await
        });
    }

    /// Walks the graph for the destinations as configured right now; a pending walk is replaced.
    fn spawn_routability_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let Some(hopr) = self.hopr.clone() else { return };
        self.cancel_routability.cancel();
        self.cancel_routability = self.cancel_on_shutdown.child_token();
        let cancel = self.cancel_routability.clone();
        let targets: Vec<(ExitKey, Address, HopRouting)> = self
            .config
            .destinations
            .iter()
            .map(|(key, dest)| (*key, dest.address, dest.routing))
            .collect();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(probe::jitter(delay)).await;
                    runner::routability(hopr, targets, results_sender).await;
                })
                .await
        });
    }

    /// Answers `probe <id>`: refuses a latched route, keeps an existing probe of the same exit, else (re)starts.
    fn probe_destination(
        &mut self,
        dest: Destination,
        results_sender: &mpsc::Sender<Results>,
    ) -> command::ProbeResponse {
        if self.hopr.is_none() {
            return command::ProbeResponse::NotReady;
        }
        if let Some(rh) = self.route_healths.get(&dest.key())
            && rh.is_unrecoverable()
        {
            return command::ProbeResponse::UnableToProbe {
                destination: dest,
                route_health: rh.state().clone(),
            };
        }
        if self.probe.as_ref().is_some_and(|p| p.key() == dest.key()) {
            return command::ProbeResponse::AlreadyProbing { destination: dest };
        }
        match self.start_probe(&dest, results_sender) {
            Some(previous) => command::ProbeResponse::Replaced {
                destination: dest,
                previous: Box::new(previous),
            },
            None => command::ProbeResponse::Probing { destination: dest },
        }
    }

    /// Starts probing `dest`, replacing any other probe. Returns the destination it replaced.
    fn start_probe(&mut self, dest: &Destination, results_sender: &mpsc::Sender<Results>) -> Option<Destination> {
        let hopr = self.hopr.clone()?;
        let replaced = self.stop_probe();
        self.probe_generation += 1;
        tracing::info!(destination = %dest, generation = self.probe_generation, "starting probe");
        self.probe = Some(Probe::start(
            dest,
            self.probe_generation,
            hopr,
            self.config.connection.clone(),
            &self.cancel_on_shutdown,
            results_sender,
        ));
        replaced
    }

    /// Dropping the probe cancels its task, which closes the session.
    fn stop_probe(&mut self) -> Option<Destination> {
        let probe = self.probe.take()?;
        tracing::info!(destination = %probe.destination(), "stopping probe");
        Some(probe.destination().clone())
    }

    /// The probe session belonging to `conn`'s exit, if that is what is being probed.
    fn probe_session_for(&self, conn: &connection::up::Up) -> Option<&SessionClientMetadata> {
        self.probe
            .as_ref()
            .filter(|p| p.key() == conn.destination.key())
            .and_then(Probe::any_session)
    }

    fn spawn_connection_runner(
        &mut self,
        destination: Destination,
        bridge_session: SessionClientMetadata,
        prev_public_key: Option<String>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let conn = connection::up::Up::new(destination.clone());
            let config_connection = self.config.connection.clone();
            let config_wireguard = self.config.wireguard.clone();
            let prev_conn = connection::up::runner::PreviousConnection {
                blokli_ips: self.cached_resolved_blokli_ips.clone(),
                wg_public_key: prev_public_key,
            };
            let spec = connection::up::runner::ConnectionSpec {
                destination: conn.destination.clone(),
                options: config_connection,
                wg_config: config_wireguard,
                bridge_session,
            };
            let pump_lifecycle = connection::up::runner::PumpLifecycle {
                cancel: self.cancel_connection.clone(),
                tasks: self.wg_pump_tasks.clone(),
            };
            let runner = connection::up::runner::Runner::new(
                spec,
                hopr.clone(),
                self.worker_params.clone(),
                prev_conn,
                pump_lifecycle,
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

    fn spawn_disconnection_runner(
        &mut self,
        disconn: &connection::down::Down,
        pump_tasks: TaskTracker,
        reuse: Option<SessionClientMetadata>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let config_connection = self.config.connection.clone();
            let runner = connection::down::runner::Runner::new(disconn.clone(), hopr, config_connection, reuse);
            let results_sender = results_sender.clone();
            self.ongoing_disconnections.push(disconn.clone());
            let outgoing_sender = self.outgoing_sender.clone();
            tokio::spawn(async move {
                wait_for_pump_stop(pump_tasks).await;
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

    fn spawn_surb_ramp_ticker(&self, results_sender: &mpsc::Sender<Results>) {
        let ramp = self.config.connection.surb_balancing.ramp;
        let cancel = self.cancel_connection.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::surb_ramp_loop(ramp, results_sender).await;
                })
                .await
        });
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

    /// Snapshot of everything `Command::Status` reports.
    fn build_status(&self) -> command::StatusResponse {
        let runmode = match self.phase.clone() {
            Phase::Initial { last_error } => RunMode::Init { last_error },
            Phase::CheckingSafe {
                node_balance,
                query_safe,
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
                let funding_tool = match self.funding_tool.clone() {
                    balance::FundingTool::NotStarted => None,
                    balance::FundingTool::InProgress => Some("Funding tool running".to_string()),
                    balance::FundingTool::CompletedSuccess(_) => Some("Funding tool ran successfully".to_string()),
                    balance::FundingTool::CompletedError(error) => Some(format!("Funding tool error: {error}")),
                };
                let error = if errors.is_empty() { None } else { Some(errors) };
                RunMode::preparing_safe(
                    self.node_address,
                    &balance,
                    funding_tool,
                    error,
                    self.minimum_balance_recommendation,
                )
            }
            Phase::DeployingSafe {
                node_balance: _,
                query_safe: _,
            } => RunMode::deploying_safe(self.node_address),
            Phase::Starting {
                edgli_init_state,
                last_error,
            } => RunMode::warmup(edgli_init_state, None, last_error),
            Phase::HoprSyncing => RunMode::warmup(None, self.hopr.as_ref().map(|h| h.status()), None),
            Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                let funding_status = match (
                    &self.ideal_balance_recommendation,
                    &self.capacity_allocations,
                    &self.balances,
                ) {
                    (Some(ideal), Some(allocs), Some(bals)) => {
                        Some(balance::to_funding_status(*ideal, allocs, bals.node_xdai))
                    }
                    _ => None,
                };
                RunMode::running(self.hopr.as_ref().map(|h| h.status()), funding_status)
            }
            Phase::ShuttingDown => RunMode::Shutdown,
        };

        let (connecting, reconnecting) = connection_infos(&self.phase, self.reconnecting_since, self.target.as_deref());
        let connected = match &self.phase {
            Phase::Connected(conn) => Some(command::ConnectedInfo {
                destination_id: conn.destination.connect_id.clone(),
                since: conn.phase.0,
                tunnel_ping_rtt: conn.tunnel_ping.rtt,
            }),
            _ => None,
        };
        let disconnecting = self
            .ongoing_disconnections
            .iter()
            .map(|d| command::DisconnectingInfo {
                destination_id: d.destination.connect_id.clone(),
                since: d.phase.0,
                phase: d.phase.1.clone(),
            })
            .collect();
        let mut vals = self.config.destinations.values().collect::<Vec<&Destination>>();
        vals.sort_unstable_by(|a, b| a.connect_id.cmp(&b.connect_id));
        let destinations = vals
            .into_iter()
            .map(|v| command::DestinationState {
                destination: v.clone(),
                route_health: self.route_healths.get(&v.key()).map(command::RouteHealthView::from),
            })
            .collect();
        command::StatusResponse {
            run_mode: runmode,
            destinations,
            target_destination: self.target.clone(),
            connecting,
            reconnecting,
            connected,
            disconnecting,
            probe: self.probe.as_ref().map(Probe::view),
        }
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    fn act_on_target(&mut self, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(target = ?self.target, phase = ?self.phase, "acting on target destination");

        // Only an absent target means disconnect; an unresolved one still waits for discovery.
        let Some(target) = self.target.clone() else {
            match self.phase.clone() {
                Phase::Connected(conn) => {
                    tracing::info!(current = %conn.destination, "disconnecting from destination");
                    self.disconnect_from_connection(&conn, results_sender);
                }
                Phase::Connecting(conn) => {
                    tracing::info!(current = %conn.destination, "disconnecting from ongoing connection attempt");
                    self.disconnect_from_connection(&conn, results_sender);
                }
                _ => {}
            }
            return;
        };
        let Some(dest) = self.config.destinations.by_connect_id(&target).cloned() else {
            tracing::debug!(%target, "target destination not known yet - waiting for discovery");
            return;
        };

        match self.phase.clone() {
            // Connecting from ready
            Phase::HoprRunning => {
                let Some(rh) = self.route_healths.get(&dest.key()) else {
                    tracing::warn!(destination = %dest, "refusing connection: destination has no route health tracker");
                    return;
                };
                match connect_step(rh, self.probe.as_ref(), dest.key()) {
                    ConnectStep::Refuse => {
                        tracing::error!(destination = %dest, route_health = ?rh.state(), "refusing connection because of route health");
                    }
                    ConnectStep::WaitRoutable => {
                        tracing::warn!(destination = %dest, route_health = ?rh.state(), "waiting for a route before connecting");
                    }
                    ConnectStep::StartProbe => {
                        tracing::info!(destination = %dest, "opening probe session before connecting");
                        self.start_probe(&dest, results_sender);
                    }
                    ConnectStep::WaitProbe => {
                        tracing::debug!(destination = %dest, "waiting for the probe's initial checks before connecting");
                    }
                    ConnectStep::Connect(bridge_session) => {
                        tracing::info!(destination = %dest, "establishing connection to new destination");
                        let prev_public_key = self.stale_wg_public_key.take();
                        self.spawn_connection_runner(dest.clone(), bridge_session, prev_public_key, results_sender);
                    }
                }
            }
            // Connecting to different destination while already connected
            Phase::Connected(conn) if !dest.same_exit(&conn.destination) => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while connected");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Connecting to different destination while already connecting
            Phase::Connecting(conn) if !dest.same_exit(&conn.destination) => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while already connecting");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // No action needed
            _ => {}
        }
    }

    fn disconnect_from_connection(&mut self, conn: &connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        self.cancel_connection.cancel();
        self.cancel_connection = self.cancel_on_shutdown.child_token();
        let pump_tasks = std::mem::replace(&mut self.wg_pump_tasks, TaskTracker::new());
        self.responders.clear();
        self.phase = Phase::HoprRunning;
        // Discovery may have dropped this exit mid-connection; its tracker was kept only for the tunnel pings.
        let configured = &self.config.destinations;
        self.route_healths.retain(|key, _| configured.contains_key(key));
        let reuse = self.probe_session_for(conn).cloned();
        if let Ok(disconn) = conn.try_into() {
            self.spawn_disconnection_runner(&disconn, pump_tasks, reuse, results_sender);
        } else {
            // connection did not even generate a wg pub key - so we can immediately try to connect again
            self.act_on_target(results_sender);
        }
    }

    /// Reconnect without a full disconnect cycle — used for ForceReconnect (WAN change).
    ///
    /// Cancels the running connection, tears down the WireGuard tunnel, then immediately
    /// spawns a new connection runner that carries the old public key so the new runner's
    /// background bridge-cleanup task can unregister it.
    async fn force_reconnect(&mut self, conn: connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        // The connection's own snapshot: the replacement runner unregisters prev_public_key here.
        let destination = conn.destination.clone();
        let prev_public_key = conn.wireguard.as_ref().map(|wg| wg.key_pair.public_key.clone());
        let bridge_session = self
            .probe
            .as_ref()
            .filter(|p| p.key() == destination.key())
            .and_then(Probe::ready_session)
            .cloned();

        self.cancel_connection.cancel();
        self.cancel_connection = self.cancel_on_shutdown.child_token();
        let pump_tasks = std::mem::replace(&mut self.wg_pump_tasks, TaskTracker::new());
        // The cancelled runner's pending root responders belong to a connection
        // that no longer exists; clear them so stale entries do not outlive it,
        // matching disconnect_from_connection.
        self.responders.clear();
        wait_for_pump_stop(pump_tasks).await;

        // this is a oneshot command and we do not wait for any result
        let _ = self
            .outgoing_sender
            .send(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
            .await;

        self.phase = Phase::HoprRunning;

        if let Some(bridge_session) = bridge_session {
            self.spawn_connection_runner(destination, bridge_session, prev_public_key, results_sender);
        } else {
            // The probe is not ready (or on another exit); the next runner unregisters the old key.
            tracing::warn!(
                ?destination,
                "force reconnect: probe session not ready - waiting for it"
            );
            self.stale_wg_public_key = prev_public_key;
            self.act_on_target(results_sender);
        }
    }

    fn on_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        self.spawn_exit_node_discovery_runner(results_sender, Duration::ZERO);
        self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::ZERO);
        self.spawn_capacity_allocations_runner(results_sender, Duration::ZERO);
        self.spawn_balances_runner(results_sender, Duration::ZERO);
        self.spawn_routability_runner(results_sender, Duration::ZERO);
        self.start_announced_peers_loop(results_sender);
        self.act_on_target(results_sender);
    }

    async fn try_start_reactor(&mut self, results_sender: &mpsc::Sender<Results>) {
        if self.strategy_handle.is_some() {
            return;
        }
        let Some(edgli) = self.hopr.as_ref() else { return };
        match edgli
            .start_telemetry_reactor(self.config.strategy.clone().into(), self.config.pix_strategy.clone())
            .await
        {
            Ok(strategy_process) => {
                tracing::info!("started edge node telemetry reactor");
                self.strategy_handle = Some(strategy_process);
            }
            Err(err) => {
                tracing::error!(?err, "failed to start edge node telemetry reactor - retrying in 10s");
                self.spawn_retry_reactor(results_sender, Duration::from_secs(10));
            }
        }
    }

    fn spawn_retry_reactor(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    let _ = results_sender.send(Results::RetryReactor).await;
                })
                .await
        });
    }
}

/// Wait for the (already cancelled) NepTUN pump task to finish so the worker's
/// TUN fd is closed before root tears down routing and drops its own fd. On
/// Linux the TUN is multi-queue: re-provisioning while a stale fd lives would
/// attach a second queue to the old device instead of creating a fresh one.
/// Connecting and reconnecting views of the phase; between attempts a reconnect has no phase.
fn connection_infos(
    phase: &Phase,
    reconnecting_since: Option<SystemTime>,
    target_dest_id: Option<&str>,
) -> (Option<command::ConnectingInfo>, Option<command::ReconnectingInfo>) {
    let reconnecting = reconnecting_since.and_then(|since| {
        let (destination_id, phase) = match phase {
            Phase::Connecting(conn) => (conn.destination.connect_id.clone(), Some(conn.phase.1.clone())),
            // Waiting on route health: the target is the only record of where we are headed.
            Phase::HoprRunning => (target_dest_id?.to_string(), None),
            _ => return None,
        };
        Some(command::ReconnectingInfo {
            destination_id,
            since,
            phase,
        })
    });
    // A reconnect supersedes the plain connecting view of the same attempt.
    let connecting = match (&reconnecting, phase) {
        (None, Phase::Connecting(conn)) => Some(command::ConnectingInfo {
            destination_id: conn.destination.connect_id.clone(),
            since: conn.phase.0,
            phase: conn.phase.1.clone(),
        }),
        _ => None,
    };
    (connecting, reconnecting)
}

/// Best-effort SURB telemetry for nerd stats; the expensive scrape is skipped when no session uses it.
fn nerd_stats_telemetry(conn: &connection::up::Up, bridge: Option<&SessionClientMetadata>) -> Option<String> {
    if bridge.is_none() && conn.ping_session.is_none() {
        return None;
    }
    match hopr::telemetry() {
        Ok(t) => Some(t),
        Err(err) => {
            tracing::warn!(?err, "failed to collect hopr telemetry for nerd stats");
            None
        }
    }
}

async fn wait_for_pump_stop(pump_tasks: TaskTracker) {
    pump_tasks.close();
    if time::timeout(Duration::from_secs(5), pump_tasks.wait()).await.is_err() {
        tracing::warn!("wg pump did not stop within 5s - proceeding with tunnel teardown");
    }
}

/// What connecting to a destination needs next, given its route and the one probe.
#[derive(Debug, PartialEq)]
enum ConnectStep {
    Refuse,
    WaitRoutable,
    StartProbe,
    WaitProbe,
    Connect(SessionClientMetadata),
}

fn connect_step(rh: &RouteHealth, probe: Option<&Probe>, key: ExitKey) -> ConnectStep {
    if rh.is_unrecoverable() {
        return ConnectStep::Refuse;
    }
    if !rh.is_routable() {
        return ConnectStep::WaitRoutable;
    }
    match probe {
        Some(p) if p.key() == key => match p.ready_session() {
            Some(session) => ConnectStep::Connect(session.clone()),
            None => ConnectStep::WaitProbe,
        },
        _ => ConnectStep::StartProbe,
    }
}

/// Trackers to restart: `Unrecoverable` latches, so a moved target needs a fresh tracker to be tried again.
fn latched_on_a_moved_target(
    destinations: &Destinations,
    route_healths: &HashMap<ExitKey, RouteHealth>,
    targets_before: &HashMap<ExitKey, (net::SocketAddr, net::SocketAddr)>,
) -> Vec<ExitKey> {
    let mut keys = Vec::new();
    for (key, dest) in destinations.iter() {
        let Some(previous) = targets_before.get(key) else {
            continue;
        };
        let target_moved = *previous != (dest.gnosis_vpn_server, dest.wireguard_server);
        let latched = route_healths.get(key).is_some_and(RouteHealth::is_unrecoverable);
        if target_moved && latched {
            keys.push(*key);
        }
    }
    keys
}

/// The live connection keeps its tracker, since tunnel pings are judged through it, until the merge after it ends.
fn trackers_to_drop(before: &HashSet<ExitKey>, after: &HashSet<ExitKey>, active: Option<ExitKey>) -> Vec<ExitKey> {
    before
        .difference(after)
        .copied()
        .filter(|key| Some(*key) != active)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{DestinationSource, HopRouting, Meta};
    use crate::connection::up::{Phase as UpPhase, Up};

    fn destination(id: &str) -> Destination {
        Destination::new(
            id.to_string(),
            Address::from([1u8; 20]),
            HopRouting::try_from(1).expect("conversion cannot fail"),
            Meta::default(),
            "172.30.0.1:8000".parse().expect("valid socket address"),
            "172.30.0.1:51820".parse().expect("valid socket address"),
            DestinationSource::Configured,
        )
    }

    fn latched_destination(id: &str) -> Destination {
        let mut dest = destination(id);
        // 0-hop without `allow_insecure` is the one latch reachable without a live health check.
        dest.routing = HopRouting::try_from(0).expect("conversion cannot fail");
        dest
    }

    fn tracker(dest: &Destination) -> RouteHealth {
        RouteHealth::new(dest, false, false)
    }

    fn targets(dest: &Destination) -> HashMap<ExitKey, (net::SocketAddr, net::SocketAddr)> {
        HashMap::from([(dest.key(), (dest.gnosis_vpn_server, dest.wireguard_server))])
    }

    fn attempt(id: &str, phase: UpPhase) -> Up {
        let mut up = Up::new(destination(id));
        up.phase = (SystemTime::UNIX_EPOCH, phase);
        up
    }

    #[test]
    fn a_vanished_exit_keeps_its_tracker_while_it_is_the_live_connection() {
        let live = destination("live").key();
        let mut far = destination("gone");
        far.routing = HopRouting::try_from(3).expect("conversion cannot fail");
        let gone = far.key();
        let before = HashSet::from([live, gone]);
        let after = HashSet::new();

        let dropped = trackers_to_drop(&before, &after, Some(live));

        assert_eq!(vec![gone], dropped);
    }

    #[test]
    fn a_kept_tracker_is_dropped_once_its_connection_is_over() {
        let gone = destination("gone").key();
        let before = HashSet::from([gone]);

        let dropped = trackers_to_drop(&before, &HashSet::new(), None);

        assert_eq!(vec![gone], dropped);
    }

    #[test]
    fn a_latched_tracker_restarts_once_discovery_moves_its_target() {
        let mut dest = latched_destination("exit");
        let before = targets(&dest);
        let route_healths = HashMap::from([(dest.key(), tracker(&dest))]);
        dest.gnosis_vpn_server = "10.0.0.1:9000".parse().expect("valid socket address");
        let destinations = Destinations::from_iter([dest]);

        let keys = latched_on_a_moved_target(&destinations, &route_healths, &before);

        assert_eq!(1, keys.len());
        assert_eq!("exit", destinations.get(&keys[0]).unwrap().connect_id);
    }

    #[test]
    fn a_latched_tracker_on_an_unchanged_target_is_left_alone() {
        let dest = latched_destination("exit");
        let before = targets(&dest);
        let route_healths = HashMap::from([(dest.key(), tracker(&dest))]);
        let destinations = Destinations::from_iter([dest]);

        let keys = latched_on_a_moved_target(&destinations, &route_healths, &before);

        assert!(keys.is_empty());
    }

    #[test]
    fn connect_step_refuses_a_latched_route() {
        let dest = latched_destination("exit");
        assert_eq!(connect_step(&tracker(&dest), None, dest.key()), ConnectStep::Refuse);
    }

    #[test]
    fn connect_step_waits_for_a_route_before_probing() {
        let dest = destination("exit");
        assert_eq!(
            connect_step(&tracker(&dest), None, dest.key()),
            ConnectStep::WaitRoutable
        );
    }

    #[test]
    fn connect_step_starts_a_probe_once_routable() {
        let dest = destination("exit");
        let mut rh = tracker(&dest);
        rh.set_routable(true);
        assert_eq!(connect_step(&rh, None, dest.key()), ConnectStep::StartProbe);
    }

    // The graph is what makes a moved target reachable again; the tracker just waits for the next walk.
    #[test]
    fn a_moved_target_alone_does_not_restart_a_live_tracker() {
        let mut dest = destination("exit");
        let before = targets(&dest);
        let route_healths = HashMap::from([(dest.key(), tracker(&dest))]);
        dest.wireguard_server = "10.0.0.1:9001".parse().expect("valid socket address");
        let destinations = Destinations::from_iter([dest]);

        let keys = latched_on_a_moved_target(&destinations, &route_healths, &before);

        assert!(keys.is_empty());
    }

    #[test]
    fn waiting_on_route_health_reports_a_reconnect_without_a_phase() {
        let since = SystemTime::UNIX_EPOCH;
        let (connecting, reconnecting) = connection_infos(&Phase::HoprRunning, Some(since), Some("exit"));
        assert!(connecting.is_none());
        let info = reconnecting.expect("the reconnect intent must be reported");
        assert_eq!(info.destination_id, "exit");
        assert_eq!(info.since, since);
        assert!(info.phase.is_none());
    }

    #[test]
    fn a_first_connect_waiting_on_route_health_is_not_a_reconnect() {
        let (connecting, reconnecting) = connection_infos(&Phase::HoprRunning, None, Some("exit"));
        assert!(connecting.is_none());
        assert!(reconnecting.is_none());
    }

    // After a worker restart the target can name a discovered exit the config has never seen.
    #[test]
    fn a_target_not_yet_published_by_discovery_is_still_reported() {
        let since = SystemTime::UNIX_EPOCH;
        let (connecting, reconnecting) = connection_infos(&Phase::HoprRunning, Some(since), Some("0xdiscovered"));
        assert!(connecting.is_none());
        let info = reconnecting.expect("the reconnect intent must survive an unresolved target");
        assert_eq!(info.destination_id, "0xdiscovered");
    }

    #[test]
    fn a_cleared_target_reports_nothing() {
        let (connecting, reconnecting) = connection_infos(&Phase::HoprRunning, Some(SystemTime::UNIX_EPOCH), None);
        assert!(connecting.is_none());
        assert!(reconnecting.is_none());
    }

    #[test]
    fn an_attempt_in_flight_reports_its_phase() {
        let phase = Phase::Connecting(attempt("exit", UpPhase::VerifyPing));
        let (connecting, reconnecting) = connection_infos(&phase, Some(SystemTime::UNIX_EPOCH), Some("exit"));
        assert!(connecting.is_none());
        let info = reconnecting.expect("the reconnect must be reported");
        assert_eq!(info.destination_id, "exit");
        assert_eq!(info.phase, Some(UpPhase::VerifyPing));
    }

    // Guards the system tests, which poll `connecting` to follow a fresh connection.
    #[test]
    fn a_fresh_connection_still_reports_connecting() {
        let phase = Phase::Connecting(attempt("exit", UpPhase::VerifyPing));
        let (connecting, reconnecting) = connection_infos(&phase, None, None);
        assert!(reconnecting.is_none());
        let info = connecting.expect("connecting must be reported");
        assert_eq!(info.destination_id, "exit");
        assert_eq!(info.phase, UpPhase::VerifyPing);
    }

    #[test]
    fn a_live_connection_never_reports_a_reconnect() {
        let phase = Phase::Connected(attempt("exit", UpPhase::ConnectionEstablished));
        let (connecting, reconnecting) = connection_infos(&phase, Some(SystemTime::UNIX_EPOCH), Some("exit"));
        assert!(connecting.is_none());
        assert!(reconnecting.is_none());
    }
}
