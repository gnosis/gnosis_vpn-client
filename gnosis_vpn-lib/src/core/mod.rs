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

use gnosis_vpn_ipc::{self as ipc, Response, RunMode};

use crate::command::{self, WorkerCommand};
use crate::compat::SafeModule;
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::{self, Address, Destination, Destinations, ExitKey, HopRouting};
use crate::event::{CoreToWorker, RequestToRoot, ResponseFromRoot, RunnerToRoot, WorkerToCore};
use crate::geoip::{self, GeoIp};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, config as hopr_config, identity};
use crate::probe::{self, Probe};
use crate::route_health::RouteHealth;
use crate::worker_params::{self, WorkerParams};
use crate::{balance, log_output, wire, wireguard};

mod channel_maintenance;
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
/// What a quick probe reports when discovery moved its exit out from under it.
const QUICK_PROBE_MOVED: &str = "destination moved while the check was running";

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
    #[error(transparent)]
    GeoIp(#[from] geoip::OpenError),
}

pub struct Core {
    // config data
    config: Config,
    geoip: GeoIp,

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
    /// Cancels the pending host-side retry tick. Separate from `cancel_connection` so that ending a
    /// cool-off never cancels the attempt the cool-off belongs to.
    cancel_retry: CancellationToken,
    // Tracks the connection's NepTUN pump task so teardown can wait for it to
    // stop (dropping its TUN fd) before asking root to tear down routing;
    // replaced together with cancel_connection.
    wg_pump_tasks: TaskTracker,

    /// One graph walk in flight at a time; replaced on every spawn.
    cancel_routability: CancellationToken,
    announced_peers_loop_running: bool,
    /// Mirrors root's `probing`: it holds the idle countdown for an explicit probe until we say the probe is gone.
    root_probe_hold: bool,

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
    /// Read handle to the strategy reactor's health, paired with `strategy_handle`.
    strategy_state: Option<edgli::StrategyStateHandle>,
    channel_maintenance: channel_maintenance::Supervisor,
    route_healths: HashMap<ExitKey, RouteHealth>,
    /// The last announced-peers snapshot, so an exit discovered between fetches gets its country at once.
    announced_ipv4: HashMap<Address, Vec<net::Ipv4Addr>>,
    /// The one long-lived probe session; connections register over it.
    probe: Option<Probe>,
    probe_generation: u64,
    /// Key and exit of a connection we dropped ourselves; the next runner to that exit resumes it.
    resume: Option<connection::up::Resume>,
    next_request_id: u64,
    // Maps a request_id to the oneshot sender waiting for root's response.
    // request_id is needed even though at most one request is in-flight at a time:
    // root runs pings in a JoinSet, so a stale ping from a cancelled connection
    // can arrive after a new responder has been stored — the id mismatch discards it.
    // Cleared in disconnect_from_connection so stale entries don't outlive their connection.
    responders: HashMap<u64, Responder>,
    ongoing_disconnections: Vec<connection::down::Down>,
    pending_unregisters: Vec<PendingUnregister>,
    reconnecting_since: Option<SystemTime>,
    /// Consecutive connection failures this host caused (not the exit). Bounds the in-place retries
    /// that keep the worker alive before falling back to a worker restart.
    host_side_failures: u32,
    /// End of the current host-side cool-off: no reconnect before it elapses. On tokio's clock, the
    /// same one the retry tick sleeps on, so the gate can never outlive its timer.
    retry_not_before: Option<time::Instant>,
}

/// How often a host-side connection failure is retried in place before the worker is rotated.
/// Each retry tears the tunnel down and re-registers, so this is generous already.
const HOST_SIDE_FAILURE_RETRIES: u32 = 3;

/// How a failed connection attempt is handled, by whose fault it failed and how often it already did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureAction {
    /// Not the target's attempt any more (its target was dropped or replaced): just record and forget.
    Ignore,
    /// The host failed before reaching the exit: rebuild the tunnel and try again after a cool-off.
    RetryHostSide,
    /// Rotate the worker - the escape hatch for everything the host-side retries could not fix.
    RestartWorker,
}

fn connection_failure_action(failed_the_target: bool, host_side: bool, host_side_failures: u32) -> FailureAction {
    if !failed_the_target {
        return FailureAction::Ignore;
    }
    if host_side && host_side_failures < HOST_SIDE_FAILURE_RETRIES {
        return FailureAction::RetryHostSide;
    }
    FailureAction::RestartWorker
}

/// Where a host-side retry tick may re-drive the target: only while it still waits, i.e. before any
/// attempt for it started (an attempt moves the phase out of `HoprRunning`).
fn retry_is_due(target: Option<&str>, phase: &Phase) -> bool {
    target.is_some() && matches!(phase, Phase::HoprRunning)
}

/// Cool-off before retrying a host-side failure. Long enough for a transient local condition (port
/// pressure, a device coming back, DNS restored) to clear, short enough to stay a fast recovery.
fn host_side_retry_delay(host_side_failures: u32) -> Duration {
    match host_side_failures {
        0 | 1 => Duration::from_secs(2),
        2 => Duration::from_secs(5),
        _ => Duration::from_secs(10),
    }
}

/// A disconnect whose unregister failed; re-run against its own destination snapshot on the next connect to this exit.
struct PendingUnregister {
    down: connection::down::Down,
    since: SystemTime,
}

/// Server GC: client_handshake_timeout_s=300 plus a 180 s sweep - older entries are already collected.
const PENDING_UNREGISTER_TTL: Duration = Duration::from_secs(8 * 60);
const PUMP_STOP_BUDGET: Duration = Duration::from_secs(5);

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
        // A database that does not open is a broken install, not a reason to run blind.
        let geoip = GeoIp::open(&config.geoip_db)?;
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
        let core = Core {
            // config data
            config,
            geoip,

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
            cancel_retry: cancel_on_shutdown.child_token(),
            wg_pump_tasks: TaskTracker::new(),
            cancel_routability: cancel_on_shutdown.child_token(),
            announced_peers_loop_running: false,
            root_probe_hold: false,

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
            strategy_state: None,
            channel_maintenance: channel_maintenance::Supervisor::default(),
            ongoing_disconnections: Vec::new(),
            pending_unregisters: Vec::new(),
            route_healths,
            announced_ipv4: HashMap::new(),
            probe: None,
            probe_generation: 0,
            resume: None,
            next_request_id: 0,
            responders: HashMap::new(),
            reconnecting_since: None,
            host_side_failures: 0,
            retry_not_before: None,
        };
        Ok((core, incoming_sender))
    }

    /// Every destination's country from the announced addresses of its exit.
    fn refresh_geo(&mut self) {
        self.config.destinations.refresh_geo(&self.announced_ipv4, &self.geoip);
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
                    if !self.on_event(event, &results_sender).await {
                        break;
                    }
                    self.sync_root_probe_hold().await;
                }

                // React to internal results from spawned runner tasks
                Some(results) = results_receiver.recv() => {
                    if !self.on_results(results, &results_sender).await {
                        break;
                    }
                    self.sync_root_probe_hold().await;
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
                        self.strategy_state = None;
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

            WorkerToCore::ForceReconnect => {
                let active_conn = match self.phase.clone() {
                    Phase::Connected(conn) => Some(conn),
                    Phase::Connecting(conn) => Some(conn),
                    _ => None,
                };
                if let Some(conn) = active_conn {
                    tracing::info!(%conn, "force reconnect triggered by WAN change");
                    self.reconnecting_since = Some(SystemTime::now());
                    self.reconnect_in_place(conn, false, results_sender).await;
                } else {
                    tracing::debug!(?self.phase, "force reconnect requested but not connected or connecting");
                }
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
                            let _ = resp.send(Response::NerdStats(Box::new(ipc::NerdStats::NoInfo {
                                tickets: ipc::TicketStatsStatus::Waiting,
                                strategies: None,
                            })));
                            return true;
                        };
                        let sender = results_sender.clone();
                        tokio::spawn(async move {
                            let ticket_stats_status = match ops.ticket_stats().await {
                                Ok(ts) => ipc::TicketStatsStatus::Available(ipc::TicketStats {
                                    ticket_price: wire::wxhopr(ts.ticket_price),
                                    winning_probability: ts.winning_probability.into(),
                                }),
                                Err(e) => ipc::TicketStatsStatus::Error { message: e.to_string() },
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
                        let _ = resp.send(Response::Status(Box::new(self.build_status())));
                    }

                    WorkerCommand::Destinations => {
                        let ids = self
                            .config
                            .destinations
                            .connect_ids()
                            .into_iter()
                            .map(Into::into)
                            .collect();
                        let _ = resp.send(Response::Destinations(ids));
                    }

                    WorkerCommand::Connect { destination: token } => {
                        match self.config.destinations.resolve(token.as_str()).cloned() {
                            Ok(dest) => {
                                // Decided before `abandon_resume`: a pending exit still holds its
                                // kept key, which a repeated Connect must not throw away.
                                let is_already_active = match &self.phase {
                                    Phase::Connected(conn) | Phase::Connecting(conn) => {
                                        conn.destination.same_exit(&dest)
                                    }
                                    // The exit is still the pending target while its cool-off runs,
                                    // and while its probe is opened once the gate is spent. The
                                    // gate alone is not enough: `act_on_target` clears it before the
                                    // probe checks, which leaves the kept key without one.
                                    Phase::HoprRunning => {
                                        self.pending_target_exit()
                                            .is_some_and(|pending| pending.same_exit(&dest))
                                            && (self.retry_not_before.is_some()
                                                || self
                                                    .resume
                                                    .as_ref()
                                                    .is_some_and(|resume| resume.destination.same_exit(&dest)))
                                    }
                                    _ => false,
                                };
                                if is_already_active {
                                    // Name the entry that is actually in use, not the one asked for:
                                    // another path to the same exit is not the pending target.
                                    let active = match &self.phase {
                                        Phase::Connected(conn) | Phase::Connecting(conn) => conn.destination.clone(),
                                        _ => self.pending_target_exit().cloned().unwrap_or_else(|| dest.clone()),
                                    };
                                    // Only an established connection is "already connected"; a pending
                                    // retry is still connecting, even though nothing new starts.
                                    let response = match &self.phase {
                                        Phase::Connected(_) => ipc::ConnectResponse::AlreadyConnected {
                                            destination: command::destination(&active),
                                        },
                                        _ => ipc::ConnectResponse::Connecting {
                                            destination: command::destination(&active),
                                        },
                                    };
                                    let _ = resp.send(Response::Connect(response));
                                    return true;
                                }
                                if let Some(rh) = self.route_healths.get(&dest.key()) {
                                    if rh.is_unrecoverable() {
                                        let _ = resp.send(Response::Connect(ipc::ConnectResponse::UnableToConnect {
                                            destination: command::destination(&dest),
                                            route_health: rh.state().clone(),
                                        }));
                                    } else if rh.is_routable() {
                                        // Opening the probe is part of connecting; act_on_target sequences it.
                                        let _ = resp.send(Response::Connect(ipc::ConnectResponse::Connecting {
                                            destination: command::destination(&dest),
                                        }));
                                        // Accepted: an earlier attempt's kept key is what this replaces.
                                        self.abandon_resume(results_sender).await;
                                        self.target = Some(dest.connect_id.clone());
                                        self.start_user_attempt();
                                        self.act_on_target(results_sender);
                                    } else {
                                        let _ = resp.send(Response::Connect(ipc::ConnectResponse::WaitingToConnect {
                                            destination: command::destination(&dest),
                                            route_health: rh.state().clone(),
                                        }));
                                        self.abandon_resume(results_sender).await;
                                        self.target = Some(dest.connect_id.clone());
                                        self.start_user_attempt();
                                    }
                                } else {
                                    tracing::warn!(key = %dest.key(), "no route health found for destination - this should not happen");
                                    let _ = resp.send(Response::Connect(ipc::ConnectResponse::DestinationNotFound));
                                }
                            }
                            Err(destination::Unresolved::Ambiguous(ids)) => {
                                tracing::info!(%token, ?ids, "connect names one exit reached by several paths");
                                let _ = resp.send(Response::Connect(ipc::ConnectResponse::DestinationAmbiguous {
                                    candidates: ids.into_iter().map(Into::into).collect(),
                                }));
                            }
                            Err(destination::Unresolved::NotFound) => {
                                tracing::info!(%token, "cannot connect to destination - not known");
                                let _ = resp.send(Response::Connect(ipc::ConnectResponse::DestinationNotFound));
                            }
                        }
                    }

                    WorkerCommand::Disconnect => {
                        self.target = None;
                        self.reconnecting_since = None;
                        self.host_side_failures = 0;
                        // A cool-off without a target must not re-drive anything: drop its tick too.
                        self.cancel_pending_retry();
                        self.abandon_resume(results_sender).await;
                        match self.phase.clone() {
                            Phase::Connected(conn) | Phase::Connecting(conn) => {
                                tracing::info!(current = %conn.destination, "disconnecting");
                                let _ = resp.send(Response::Disconnect(ipc::DisconnectResponse::Disconnecting {
                                    destination: command::destination(&conn.destination),
                                }));
                            }
                            _ => {
                                tracing::debug!("no active connection to disconnect");
                                let _ = resp.send(Response::Disconnect(ipc::DisconnectResponse::NotConnected));
                            }
                        }
                        self.act_on_target(results_sender);
                    }

                    WorkerCommand::Probe { destination: token } => {
                        let response = match self.config.destinations.resolve(token.as_str()).cloned() {
                            Ok(dest) => self.probe_destination(dest, results_sender),
                            Err(destination::Unresolved::Ambiguous(ids)) => ipc::ProbeResponse::DestinationAmbiguous {
                                candidates: ids.into_iter().map(Into::into).collect(),
                            },
                            Err(destination::Unresolved::NotFound) => ipc::ProbeResponse::DestinationNotFound,
                        };
                        let _ = resp.send(Response::Probe(response));
                    }

                    WorkerCommand::QuickProbe { destination: token } => {
                        match self.config.destinations.resolve(token.as_str()).cloned() {
                            Ok(dest) => self.spawn_quick_probe(dest, resp, results_sender),
                            Err(destination::Unresolved::Ambiguous(ids)) => {
                                let _ =
                                    resp.send(Response::QuickProbe(ipc::QuickProbeResponse::DestinationAmbiguous {
                                        candidates: ids.into_iter().map(Into::into).collect(),
                                    }));
                            }
                            Err(destination::Unresolved::NotFound) => {
                                let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::DestinationNotFound));
                            }
                        }
                    }

                    WorkerCommand::Unprobe => {
                        let response = match self.probe.as_ref() {
                            None => {
                                self.root_probe_hold = false;
                                ipc::UnprobeResponse::NotProbing
                            }
                            Some(p) if self.probe_in_use(p.destination()) => ipc::UnprobeResponse::InUse {
                                destination: command::destination(p.destination()),
                            },
                            Some(p) => {
                                let destination = p.destination().clone();
                                self.stop_probe();
                                self.root_probe_hold = false;
                                ipc::UnprobeResponse::Closing {
                                    destination: command::destination(&destination),
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
                                        (Some(ideal), Some(allocs)) => Some(balance::to_funding_status(
                                            *ideal,
                                            allocs,
                                            balances.node_xdai,
                                            balances.safe_wxhopr,
                                        )),
                                        _ => None,
                                    };
                                ipc::BalanceResponse::Available(Box::new(command::balances(
                                    &hopr.info(),
                                    balances,
                                    &self.config.destinations,
                                    self.capacity_allocations.as_ref(),
                                    self.ideal_balance_recommendation,
                                    funding_status,
                                )))
                            }
                            _ => ipc::BalanceResponse::Unavailable {
                                reason: "balance data not yet available".to_string(),
                            },
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

                    WorkerCommand::FundingTool { secret } => {
                        let in_presafe_phase = matches!(self.phase, Phase::CheckingSafe { .. });
                        let rerun_allowed = self.worker_params.allow_funding_tool_rerun();
                        // cooldown only gates reruns; without the flag, a completed run stays Done forever
                        let cooldown_remaining =
                            rerun_allowed.then(|| self.funding_tool.cooldown_remaining()).flatten();

                        let response = if !(in_presafe_phase || rerun_allowed) {
                            ipc::FundingToolResponse::WrongPhase
                        } else if let Some(remaining) = cooldown_remaining {
                            ipc::FundingToolResponse::Cooldown { remaining }
                        } else {
                            match &self.funding_tool {
                                balance::FundingTool::InProgress => ipc::FundingToolResponse::InProgress,
                                balance::FundingTool::CompletedSuccess(_) if !rerun_allowed => {
                                    ipc::FundingToolResponse::Done
                                }
                                balance::FundingTool::NotStarted
                                | balance::FundingTool::CompletedError(_)
                                | balance::FundingTool::CompletedSuccess(_) => {
                                    self.funding_tool = balance::FundingTool::InProgress;
                                    self.spawn_funding_runner(secret.expose().to_string(), results_sender);
                                    ipc::FundingToolResponse::Started
                                }
                            }
                        };
                        let _ = resp.send(Response::FundingTool(response));
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
                self.merge_discovered_destinations(nodes, results_sender).await;
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
                    self.spawn_strategy_sample_runner(results_sender, Duration::ZERO);
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
                    self.announced_ipv4 = announced
                        .iter()
                        .map(|(address, peer)| (*address, peer.ipv4_addrs.clone()))
                        .collect();
                    self.refresh_geo();
                    let _ = self
                        .outgoing_sender
                        .send(CoreToWorker::RequestToRoot(RequestToRoot::UpdatePeerIps { peer_ips }))
                        .await;
                }
                Err(err) => tracing::error!(?err, "failed to fetch announced peers"),
            },

            Results::Routability { map } => {
                let mut became_routable = false;
                for (key, walked) in map {
                    if let Some(rh) = self.route_healths.get_mut(&key) {
                        match walked {
                            Ok(walk) => became_routable |= rh.apply_walk(walk),
                            Err(err) => rh.with_error(err),
                        }
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
                    self.host_side_failures = 0;
                    conn.connected();
                    self.phase = Phase::Connected(conn.clone());
                    let route = format!(
                        "{}({})",
                        conn.destination.pretty_print_path(),
                        wire::address(&conn.destination.address).short()
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
                    // By identity: a dropped target must still restart the worker, not stick in Connecting.
                    let failed_the_target = self.target.as_deref() == Some(conn.destination.connect_id.as_str());
                    match connection_failure_action(failed_the_target, err.is_host_side(), self.host_side_failures) {
                        FailureAction::Ignore => {
                            self.reconnecting_since = None;
                            if let Some(rh) = self.route_healths.get_mut(&conn.destination.key()) {
                                rh.with_error(err.to_string());
                            }
                        }
                        // The exit never saw the attempt, so its route is not at fault.
                        FailureAction::RetryHostSide => {
                            self.host_side_failures += 1;
                            let delay = host_side_retry_delay(self.host_side_failures);
                            // One absolute deadline for the gate and the timer, taken before the
                            // teardown so the cool-off starts now, not once the teardown finished.
                            let retry_at = time::Instant::now() + delay;
                            tracing::warn!(
                                ?err,
                                %conn,
                                attempt = self.host_side_failures,
                                max = HOST_SIDE_FAILURE_RETRIES,
                                ?delay,
                                "connection failed on the host side - rebuilding the tunnel, then retrying"
                            );
                            // Kept across the cool-off so status keeps reporting a reconnect.
                            self.reconnecting_since.get_or_insert_with(SystemTime::now);
                            self.retry_not_before = Some(retry_at);
                            self.begin_reconnect(conn, false).await;
                            self.spawn_retry_connection(retry_at, results_sender);
                        }
                        FailureAction::RestartWorker => {
                            self.reconnecting_since = None;
                            if let Some(rh) = self.route_healths.get_mut(&conn.destination.key()) {
                                rh.with_error(err.to_string());
                            }
                            tracing::info!(id = %conn.destination.connect_id, "restarting connection worker process due to final connection error");
                            return false;
                        }
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
                        self.pending_unregisters
                            .retain(|p| p.down.wg_public_key != wg_public_key);
                    }
                    Err(err) => {
                        tracing::warn!(?err, %wg_public_key, "disconnect failed - retrying on next connect to this exit");
                        let down = self
                            .ongoing_disconnections
                            .iter()
                            .find(|c| c.wg_public_key == wg_public_key);
                        // A failed retry keeps its first entry, so the TTL counts from the original failure.
                        let already_pending = self
                            .pending_unregisters
                            .iter()
                            .any(|p| p.down.wg_public_key == wg_public_key);
                        if let (Some(down), false) = (down, already_pending) {
                            self.pending_unregisters.push(PendingUnregister {
                                down: down.clone(),
                                since: SystemTime::now(),
                            });
                        }
                    }
                }
                self.ongoing_disconnections.retain(|c| c.wg_public_key != wg_public_key);
                self.act_on_target(results_sender);
            }

            Results::WgPumpExited { reason, device_lost } => match self.phase.clone() {
                Phase::Connected(conn) => {
                    tracing::warn!(%conn, %reason, device_lost, "wg pump exited - reconnecting");
                    self.reconnecting_since = Some(SystemTime::now());
                    self.reconnect_in_place(conn, !device_lost, results_sender).await;
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
                    let max_rtt = self.config.connection.health_check_intervals.tunnel_ping_max_rtt;
                    let failures = conn.tunnel_ping_result(rtt, max_rtt);
                    self.phase = Phase::Connected(conn.clone());
                    let max = self.config.connection.health_check_intervals.tunnel_ping_max_failures;
                    if failures >= max {
                        tracing::warn!(%conn, failures, "tunnel ping exceeded max failures - reconnecting");
                        self.reconnecting_since = Some(SystemTime::now());
                        self.reconnect_in_place(conn, true, results_sender).await;
                    }
                }
            }

            Results::WgStatsSample(sample) => match self.phase.clone() {
                Phase::Connecting(mut conn) => {
                    conn.record_wg_stats(sample);
                    self.phase = Phase::Connecting(conn);
                }
                Phase::Connected(mut conn) => {
                    let stall = conn.record_wg_stats(sample);
                    self.phase = Phase::Connected(conn.clone());
                    if stall >= connection::up::COUNTER_STALL_RECONNECT {
                        tracing::warn!(%conn, ?stall, "inbound counters stalled - reconnecting");
                        self.reconnecting_since = Some(SystemTime::now());
                        self.reconnect_in_place(conn, true, results_sender).await;
                    }
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

            Results::RetryConnection { not_before } => {
                // The timer is authoritative for its own cool-off: it clears the gate and re-drives,
                // but only if no newer cool-off replaced the deadline it was scheduled for.
                // A tick that was already queued when a newer cool-off was armed must not end it.
                if self.retry_not_before == Some(not_before) {
                    self.retry_not_before = None;
                }
                if retry_is_due(self.target.as_deref(), &self.phase) {
                    tracing::info!("retrying the connection after a host-side failure");
                    self.act_on_target(results_sender);
                } else {
                    tracing::debug!(
                        target = ?self.target,
                        phase = ?self.phase,
                        gate = ?self.retry_not_before,
                        "host-side retry tick outside a waiting phase or superseded"
                    );
                }
            }

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
                if let probe::Event::OpenAborted { error } = &event {
                    let key = probe.key();
                    tracing::error!(destination = %probe.destination(), %error, "probe cannot open a session - stopping it");
                    self.stop_probe();
                    // The same config aborts every retry, so latch it - a restart would only loop, and nothing else re-drives the target.
                    if let Some(rh) = self.route_healths.get_mut(&key) {
                        rh.set_cannot_open_session(error.clone());
                    }
                    return true;
                }
                if let probe::Event::OpenGaveUp { error } = &event {
                    let key = probe.key();
                    tracing::error!(destination = %probe.destination(), %error, "probe gave up opening a session - stopping it");
                    self.stop_probe();
                    if let Some(rh) = self.route_healths.get_mut(&key) {
                        rh.with_error(error.clone());
                    }
                    // Root keeps the killswitch and the target across a restart, so a fresh worker is the last thing left to try.
                    return self.target_key() != Some(key);
                }
                let key = probe.key();
                let was_ready = probe.ready_session().is_some();
                let reopening = matches!(event, probe::Event::Reopening { .. });
                if let probe::Event::Version { versions, .. } = &event
                    && let Some(rh) = self.route_healths.get_mut(&key)
                {
                    rh.apply_api_versions(&versions.api);
                }
                probe.apply(event);
                let is_ready = probe.ready_session().is_some();
                if !was_ready && is_ready {
                    self.act_on_target(results_sender);
                }
                // A connection still registering holds a bound_host that just died; restart in place so its key and device survive.
                if reopening
                    && let Phase::Connecting(conn) = self.phase.clone()
                    && conn.destination.key() == key
                    && conn.registration.is_none()
                {
                    tracing::warn!(%conn, "probe session broke during registration - restarting the attempt");
                    self.reconnect_in_place(conn, true, results_sender).await;
                }
            }

            Results::RetryReactor => {
                self.try_start_reactor(results_sender).await;
            }

            Results::StrategySampleTick => {
                let sample = self.strategy_state.as_ref().map(|h| h.state());
                self.channel_maintenance.on_sample(sample, SystemTime::now());
                self.spawn_strategy_sample_runner(results_sender, channel_maintenance::SAMPLE_INTERVAL);
            }

            Results::QuickProbe {
                destination,
                outcome,
                session,
            } => {
                // Nobody waits on a quick probe anymore, so a failure would otherwise only show up in `status`.
                if let Err(error) = &outcome {
                    tracing::warn!(%destination, %error, "quick probe failed");
                }
                // Discovery may have moved this exit mid-check, leaving result and session on the old server.
                let moved = probe_is_stale(self.config.destinations.get(&destination.key()), &destination);
                let (outcome, session) = match moved {
                    true => (Err(QUICK_PROBE_MOVED.to_string()), None),
                    false => (outcome, session),
                };
                match self.route_healths.get_mut(&destination.key()) {
                    Some(rh) => rh.apply_quick_probe(&outcome, SystemTime::now()),
                    None => tracing::debug!(%destination, "quick probe finished for a destination that is gone"),
                }
                match self.probe.as_mut().filter(|p| p.key() == destination.key()) {
                    // `None` tells a waiting probe task to open its own session.
                    Some(probe) => probe.adopt(session),
                    // Nobody wants it; ProbeSession's Drop closes it.
                    None => drop(session),
                }
            }

            Results::NerdStatsTicketStats {
                res: ticket_stats_status,
                resp,
            } => {
                let telemetry = nerd_stats_telemetry();
                let strategies = self
                    .strategy_state
                    .as_ref()
                    .map(|h| command::strategy_stats(telemetry.as_deref().unwrap_or_default(), h.state()));
                let stats = match &self.phase {
                    Phase::Connecting(conn) => {
                        let bridge = self.probe_session_for(conn);
                        let conn = command::conn_stats(conn, self.node_address, telemetry.as_deref(), bridge);
                        ipc::NerdStats::Connecting {
                            tickets: ticket_stats_status,
                            strategies,
                            conn,
                        }
                    }
                    Phase::Connected(conn) => {
                        let bridge = self.probe_session_for(conn);
                        let conn = command::conn_stats(conn, self.node_address, telemetry.as_deref(), bridge);
                        ipc::NerdStats::Connected {
                            tickets: ticket_stats_status,
                            strategies,
                            conn,
                        }
                    }
                    _ => ipc::NerdStats::NoInfo {
                        tickets: ticket_stats_status,
                        strategies,
                    },
                };
                let _ = resp.send(Response::NerdStats(Box::new(stats)));
            }
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
    async fn merge_discovered_destinations(
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
        self.refresh_geo();
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

        let probe_stale = self.probe.as_ref().is_some_and(|p| {
            // An explicit probe holds its own destination like a connection does; only a connect-owned one follows discovery.
            let droppable = !p.is_requested() && !self.probe_session_busy(p.destination());
            droppable && probe_is_stale(self.config.destinations.get(&p.key()), p.destination())
        });
        if probe_stale {
            tracing::info!("probed exit gone or moved in discovery - closing its session");
            self.stop_probe();
            // A target waiting on that probe needs a fresh one against the current endpoints.
            self.act_on_target(results_sender);
        }

        // A tracker that just started over needs a graph walk now, not on the lazy tick.
        if !fresh.is_empty() {
            self.spawn_routability_runner(results_sender, Duration::ZERO);
        }
    }

    async fn determine_next_phase_from_safe_disk_query(&mut self, results_sender: &mpsc::Sender<Results>) {
        let res = hopr_config::read_safe(&self.worker_params.network_cache_dir()).await;
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
        let network_cache_dir = self.worker_params.network_cache_dir();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::persist_safe(network_cache_dir, safe_module, results_sender).await;
                })
                .await
        });
    }

    fn spawn_hopr_runner(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let hopr_config_options = self.config.connection.hopr_config();
        let probe_local_addresses = self.config.connection.probe_local_addresses;
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::hopr(
                        worker_params,
                        blokli_config,
                        hopr_config_options,
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
    fn probe_destination(&mut self, dest: Destination, results_sender: &mpsc::Sender<Results>) -> ipc::ProbeResponse {
        if self.hopr.is_none() {
            return ipc::ProbeResponse::NotReady;
        }
        if let Some(rh) = self.route_healths.get(&dest.key())
            && rh.is_not_allowed()
        {
            return ipc::ProbeResponse::UnableToProbe {
                destination: command::destination(&dest),
                route_health: rh.state().clone(),
            };
        }
        // Root counts this answer towards the idle countdown, so the probe must outlive the target too.
        if let Some(probe) = self.probe.as_mut().filter(|p| p.key() == dest.key()) {
            probe.mark_requested();
            self.root_probe_hold = true;
            return ipc::ProbeResponse::AlreadyProbing {
                destination: command::destination(&dest),
            };
        }
        if let Some(in_use) = self
            .probe
            .as_ref()
            .map(Probe::destination)
            .filter(|probed| self.probe_in_use(probed))
        {
            return ipc::ProbeResponse::InUse {
                destination: command::destination(in_use),
            };
        }
        self.root_probe_hold = true;
        match self.start_probe(&dest, true, results_sender) {
            Some(previous) => ipc::ProbeResponse::Replaced {
                destination: command::destination(&dest),
                previous: command::destination(&previous),
            },
            None => ipc::ProbeResponse::Probing {
                destination: command::destination(&dest),
            },
        }
    }

    /// Starts `quickprobe <id>` and answers right away; the result lands in the destination's route health.
    fn spawn_quick_probe(
        &mut self,
        dest: Destination,
        resp: oneshot::Sender<Response>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        let Some(hopr) = self.hopr.clone() else {
            let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::NotReady));
            return;
        };
        let Some(route_health) = self.route_healths.get_mut(&dest.key()) else {
            tracing::warn!(key = %dest.key(), "no route health found for destination - this should not happen");
            let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::DestinationNotFound));
            return;
        };
        if !route_health.is_routable() && !route_health.is_incompatible_api() {
            let state = route_health.state().clone();
            let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::UnableToProbe {
                destination: command::destination(&dest),
                route_health: state,
            }));
            return;
        }
        // The long-lived probe already reports on this exit; a second session would only add load.
        if self.probe.as_ref().is_some_and(|p| p.key() == dest.key()) {
            let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::AlreadyProbing {
                destination: command::destination(&dest),
            }));
            return;
        }
        if !route_health.start_quick_probe(SystemTime::now()) {
            let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::AlreadyChecking {
                destination: command::destination(&dest),
            }));
            return;
        }
        let _ = resp.send(Response::QuickProbe(ipc::QuickProbeResponse::Checking {
            destination: command::destination(&dest),
        }));

        let options = self.config.connection.clone();
        let cancel = self.cancel_on_shutdown.clone();
        let sender = results_sender.clone();
        tokio::spawn(async move {
            let result = cancel
                .run_until_cancelled(probe::quick_probe(hopr, dest.clone(), options))
                .await;
            let Some((outcome, session)) = result else {
                tracing::debug!(destination = %dest, "quick probe cancelled by shutdown");
                return;
            };
            let _ = sender
                .send(Results::QuickProbe {
                    destination: Box::new(dest),
                    outcome,
                    session,
                })
                .await;
        });
    }

    /// The exit the current target resolves to, once discovery knows it.
    fn target_key(&self) -> Option<ExitKey> {
        let target = self.target.as_ref()?;
        self.config.destinations.by_connect_id(target).map(Destination::key)
    }

    /// Whether someone is mid-exchange over the probe's session right now.
    fn probe_session_busy(&self, probed: &Destination) -> bool {
        probe_session_busy(&self.phase, &self.ongoing_disconnections, probed)
    }

    /// Whether dropping the probe of `probed` would pull the session out from under someone.
    fn probe_in_use(&self, probed: &Destination) -> bool {
        probe_in_use(&self.phase, &self.ongoing_disconnections, self.target_key(), probed)
    }

    /// Starts probing `dest`, replacing any other probe. Returns the destination it replaced.
    fn start_probe(
        &mut self,
        dest: &Destination,
        requested: bool,
        results_sender: &mpsc::Sender<Results>,
    ) -> Option<Destination> {
        let hopr = self.hopr.clone()?;
        let replaced = self.stop_probe();
        self.probe_generation += 1;
        let start = probe::Start {
            requested,
            // A quick check in flight for this exit already holds a session; the probe takes it over.
            adopt_quick_session: self
                .route_healths
                .get(&dest.key())
                .is_some_and(RouteHealth::is_quick_probing),
        };
        tracing::info!(
            destination = %dest,
            generation = self.probe_generation,
            adopting = start.adopt_quick_session,
            "starting probe"
        );
        self.probe = Some(Probe::start(
            dest,
            self.probe_generation,
            hopr,
            self.config.connection.clone(),
            &self.cancel_on_shutdown,
            results_sender,
            start,
        ));
        replaced
    }

    /// Root holds the idle countdown for an explicit probe; every way that probe can vanish ends up here.
    async fn sync_root_probe_hold(&mut self) {
        let held = self.probe.as_ref().is_some_and(Probe::is_requested);
        if !self.root_probe_hold || held {
            return;
        }
        self.root_probe_hold = false;
        let request = CoreToWorker::RequestToRoot(RequestToRoot::ProbeStopped);
        let _ = self.outgoing_sender.send(request).await;
    }

    /// Closes a probe that only existed for a target that is now gone; a requested one stays.
    fn stop_implicit_probe(&mut self) {
        let orphaned = self
            .probe
            .as_ref()
            .is_some_and(|p| !p.is_requested() && !self.probe_in_use(p.destination()));
        if orphaned {
            self.stop_probe();
        }
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
        results_sender: &mpsc::Sender<Results>,
    ) {
        // A user-initiated switch already abandoned it; this only guards the invariant.
        let mut resume = self.resume.take();
        if let Some(stale) = resume.take_if(|r| !r.destination.same_exit(&destination)) {
            tracing::warn!(resume = %stale, %destination, "dropping a resume kept for another exit");
            self.spawn_unregister_retry(&stale.into(), results_sender);
        }
        self.retry_pending_unregisters(&destination, results_sender);
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let mut conn = connection::up::Up::new(destination.clone());
            // Seeded so a disconnect before the runner reports its key still unregisters and tears down.
            if let Some(resume) = &resume {
                conn.wireguard = Some(resume.wireguard.clone());
            }
            let config_connection = self.config.connection.clone();
            let config_wireguard = self.config.wireguard.clone();
            let prev_conn = connection::up::runner::PreviousConnection { resume };
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
            let runner = connection::up::runner::Runner::new(spec, hopr.clone(), prev_conn, pump_lifecycle);
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

    /// Re-runs the failed disconnects of this exit; entries leave only on success or past the server's GC window.
    fn retry_pending_unregisters(&mut self, destination: &Destination, results_sender: &mpsc::Sender<Results>) {
        let now = SystemTime::now();
        let still_registered =
            |p: &PendingUnregister| now.duration_since(p.since).unwrap_or_default() < PENDING_UNREGISTER_TTL;
        self.pending_unregisters.retain(still_registered);
        let retries: Vec<connection::down::Down> = self
            .pending_unregisters
            .iter()
            .filter(|p| p.down.destination.same_exit(destination))
            .map(|p| p.down.clone())
            .collect();
        for down in retries {
            let already_running = self
                .ongoing_disconnections
                .iter()
                .any(|c| c.wg_public_key == down.wg_public_key);
            if already_running {
                continue;
            }
            tracing::info!(%down, "retrying failed disconnect");
            self.spawn_unregister_retry(&down, results_sender);
        }
    }

    /// Like spawn_disconnection_runner without the tunnel teardown - the retry has no live tunnel.
    fn spawn_unregister_retry(&mut self, disconn: &connection::down::Down, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let config_connection = self.config.connection.clone();
            let runner = connection::down::runner::Runner::new(disconn.clone(), hopr, config_connection, None);
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
    fn build_status(&self) -> ipc::Status {
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
                command::preparing_safe(
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
            } => command::deploying_safe(self.node_address),
            Phase::Starting {
                edgli_init_state,
                last_error,
            } => command::warmup(edgli_init_state, None, last_error),
            Phase::HoprSyncing => command::warmup(None, self.hopr.as_ref().map(|h| h.status()), None),
            Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                let funding_status = match (
                    &self.ideal_balance_recommendation,
                    &self.capacity_allocations,
                    &self.balances,
                ) {
                    (Some(ideal), Some(allocs), Some(bals)) => Some(balance::to_funding_status(
                        *ideal,
                        allocs,
                        bals.node_xdai,
                        bals.safe_wxhopr,
                    )),
                    _ => None,
                };
                command::running(
                    self.hopr.as_ref().map(|h| h.status()),
                    funding_status,
                    self.channel_maintenance.health(),
                )
            }
            Phase::ShuttingDown => RunMode::Shutdown,
        };

        let reconnect_at = self.config.connection.health_check_intervals.tunnel_ping_max_failures;
        let pix = self.main_session_pix();
        let connection = connection_view(
            &self.phase,
            self.reconnecting_since,
            self.target.as_deref(),
            reconnect_at,
            pix,
        );
        let disconnecting = self
            .ongoing_disconnections
            .iter()
            .map(|d| ipc::Disconnecting {
                destination: d.destination.connect_id.as_str().into(),
                since: d.phase.0,
                phase: d.phase.1.clone(),
            })
            .collect();
        let mut vals = self.config.destinations.values().collect::<Vec<&Destination>>();
        vals.sort_unstable_by(|a, b| a.connect_id.cmp(&b.connect_id));
        let destinations = vals
            .into_iter()
            .map(|v| ipc::DestinationStatus {
                destination: v.into(),
                route_health: self.route_healths.get(&v.key()).map(RouteHealth::view),
            })
            .collect();
        ipc::Status {
            run_mode: runmode,
            destinations,
            target: self.target.as_deref().map(Into::into),
            connection,
            disconnecting,
            probe: self.probe.as_ref().map(Probe::view),
            pix_chunk_bytes: self.hopr.as_ref().map(|h| h.pix_chunk_bytes()),
        }
    }

    /// The ping session becomes the main one, so it is the session that pays the exit.
    fn main_session_pix(&self) -> Option<ipc::PixChunkStats> {
        let conn = match &self.phase {
            Phase::Connecting(conn) | Phase::Connected(conn) => conn,
            _ => return None,
        };
        let hopr = self.hopr.as_ref()?;
        let (_, session) = conn.ping_session.as_ref()?;
        let session_id = session.active_clients.first()?;
        let price_per_byte = self.config.pix_strategy.price_per_byte;
        Some(hopr.pix_chunks().stats(session_id, price_per_byte))
    }

    /// A user-requested attempt starts with a fresh host-side budget and no cool-off; a repeated
    /// Connect against an attempt that is already running must not refill it.
    fn start_user_attempt(&mut self) {
        self.host_side_failures = 0;
        self.reconnecting_since = None;
        // Also drops the previous target's retry tick, so it cannot re-drive this one.
        self.cancel_pending_retry();
    }

    /// The exit the pending target names, if discovery still knows it.
    fn pending_target_exit(&self) -> Option<&Destination> {
        self.target
            .as_deref()
            .and_then(|id| self.config.destinations.by_connect_id(id))
    }

    /// End a scheduled host-side retry: the gate goes and the pending tick loses its token.
    fn cancel_pending_retry(&mut self) {
        self.retry_not_before = None;
        self.cancel_retry.cancel();
        self.cancel_retry = self.cancel_on_shutdown.child_token();
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
            // Root never counted this one towards the idle countdown, so stopping it needs no message.
            self.stop_implicit_probe();
            return;
        };
        let Some(dest) = self.config.destinations.by_connect_id(&target).cloned() else {
            tracing::debug!(%target, "target destination not known yet - waiting for discovery");
            return;
        };

        match self.phase.clone() {
            // Connecting from ready
            Phase::HoprRunning => {
                // A host-side failure holds the next attempt back; `Results::RetryConnection` re-runs
                // this once the cool-off elapsed, so anything else arriving meanwhile just waits.
                if let Some(not_before) = self.retry_not_before
                    && time::Instant::now() < not_before
                {
                    tracing::debug!(?not_before, %dest, "waiting for the host-side cool-off before reconnecting");
                    return;
                }
                self.retry_not_before = None;
                let Some(rh) = self.route_healths.get(&dest.key()) else {
                    tracing::warn!(destination = %dest, "refusing connection: destination has no route health tracker");
                    return;
                };
                match connect_step(rh, self.probe.as_ref(), &dest) {
                    ConnectStep::Refuse => {
                        tracing::error!(destination = %dest, route_health = ?rh.state(), "refusing connection because of route health");
                    }
                    ConnectStep::WaitRoutable => {
                        tracing::warn!(destination = %dest, route_health = ?rh.state(), "waiting for a route before connecting");
                    }
                    ConnectStep::StartProbe => {
                        // A disconnect may still be unregistering over the current probe; its result re-runs this.
                        if let Some(busy) = self
                            .probe
                            .as_ref()
                            .map(Probe::destination)
                            .filter(|probed| self.probe_session_busy(probed))
                        {
                            tracing::debug!(destination = %dest, %busy, "waiting for the ongoing disconnect before replacing its probe");
                            return;
                        }
                        tracing::info!(destination = %dest, "opening probe session before connecting");
                        // Replacing this exit's own stale probe must not demote an explicit one to connect-owned.
                        let requested = self
                            .probe
                            .as_ref()
                            .is_some_and(|p| p.key() == dest.key() && p.is_requested());
                        self.start_probe(&dest, requested, results_sender);
                    }
                    ConnectStep::WaitProbe => {
                        tracing::debug!(destination = %dest, "waiting for the probe's initial checks before connecting");
                    }
                    ConnectStep::Connect(bridge_session) => {
                        tracing::info!(destination = %dest, "establishing connection to new destination");
                        self.spawn_connection_runner(dest.clone(), bridge_session, results_sender);
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

    /// Every reconnect we trigger ourselves keeps key and registration; only the user rotates them.
    async fn reconnect_in_place(
        &mut self,
        conn: connection::up::Up,
        keep_device: bool,
        results_sender: &mpsc::Sender<Results>,
    ) {
        self.begin_reconnect(conn, keep_device).await;
        self.act_on_target(results_sender);
    }

    /// Abandon the running attempt and, unless the device is kept, tear the tunnel down. Does not
    /// dispatch the next attempt - callers choose whether to reconnect now or after a cool-off.
    async fn begin_reconnect(&mut self, conn: connection::up::Up, keep_device: bool) {
        self.cancel_connection.cancel();
        self.cancel_connection = self.cancel_on_shutdown.child_token();
        let pump_tasks = std::mem::replace(&mut self.wg_pump_tasks, TaskTracker::new());
        self.responders.clear();
        self.phase = Phase::HoprRunning;
        // Discovery may have dropped this exit mid-connection; its tracker was kept only for the tunnel pings.
        let configured = &self.config.destinations;
        self.route_healths.retain(|key, _| configured.contains_key(key));

        // A pump still holding the TUN fd would compete with the next one for packets on a reused device.
        let pump_stopped = wait_for_pump_stop(pump_tasks).await;
        if !keep_device || !pump_stopped {
            // Awaited so root sees it before the next runner's SetupTunnel.
            let _ = self
                .outgoing_sender
                .send(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
                .await;
        }

        match connection::up::Resume::try_from(&conn) {
            Ok(resume) => {
                tracing::info!(%resume, keep_device, "reconnecting in place");
                self.resume = Some(resume);
            }
            Err(reason) => tracing::debug!(reason, "reconnecting with a fresh key"),
        }
    }

    /// Retry the target once the host-side cool-off elapsed.
    ///
    /// Bound to its own token, not the attempt's: a newer failure rotates it, a user Connect or
    /// Disconnect cancels it, so the tick never outlives the cool-off it belongs to. The
    /// `not_before` equality check is the second line of defence.
    fn spawn_retry_connection(&mut self, retry_at: time::Instant, results_sender: &mpsc::Sender<Results>) {
        // A newer failure supersedes the older tick: rotate the token so the old sleep is cancelled.
        self.cancel_retry.cancel();
        self.cancel_retry = self.cancel_on_shutdown.child_token();
        let sender = results_sender.clone();
        let cancel = self.cancel_retry.clone();
        tokio::spawn(async move {
            let tick = async move {
                time::sleep_until(retry_at).await;
                let _ = sender.send(Results::RetryConnection { not_before: retry_at }).await;
            };
            let _ = cancel.run_until_cancelled(tick).await;
        });
    }

    /// A user action rotates the key: the kept registration is removed at the exit and the device torn down.
    async fn abandon_resume(&mut self, results_sender: &mpsc::Sender<Results>) {
        let Some(resume) = self.resume.take() else { return };
        tracing::info!(%resume, "explicit command replaces pending reconnect - new key");
        // Awaited: after a disconnect no later SetupTunnel would replace the kept device.
        if let Err(err) = self
            .outgoing_sender
            .send(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
            .await
        {
            tracing::warn!(%err, "failed to request tunnel teardown for abandoned reconnect");
        }
        self.spawn_unregister_retry(&resume.into(), results_sender);
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
            .start_telemetry_reactor(
                self.config.strategy.clone().into(),
                self.config
                    .pix_strategy
                    .to_entry_config(self.worker_params.pix_state_dir()),
            )
            .await
        {
            Ok(reactor) => {
                tracing::info!("started edge node telemetry reactor");
                self.strategy_handle = Some(reactor.abort_handle);
                self.strategy_state = Some(reactor.strategy_state);
            }
            Err(err) => {
                tracing::error!(?err, "failed to start edge node telemetry reactor - retrying in 10s");
                self.spawn_retry_reactor(results_sender, Duration::from_secs(10));
            }
        }
    }

    fn spawn_strategy_sample_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    let _ = results_sender.send(Results::StrategySampleTick).await;
                })
                .await
        });
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

/// The one connection the phase describes; between attempts a reconnect has no phase.
fn connection_view(
    phase: &Phase,
    reconnecting_since: Option<SystemTime>,
    target_dest_id: Option<&str>,
    reconnect_at: u32,
    pix: Option<ipc::PixChunkStats>,
) -> ipc::Connection {
    if let Some(since) = reconnecting_since {
        let reconnecting = match phase {
            Phase::Connecting(conn) => Some((conn.destination.connect_id.as_str(), Some(conn.phase.1.clone()))),
            // Waiting on route health: the target is the only record of where we are headed.
            Phase::HoprRunning => target_dest_id.map(|id| (id, None)),
            _ => None,
        };
        if let Some((destination, phase)) = reconnecting {
            return ipc::Connection::Reconnecting {
                destination: destination.into(),
                since,
                phase,
            };
        }
    }
    match phase {
        Phase::Connecting(conn) => ipc::Connection::Connecting {
            destination: conn.destination.connect_id.as_str().into(),
            since: conn.phase.0,
            phase: conn.phase.1.clone(),
            pix,
        },
        Phase::Connected(conn) => ipc::Connection::Connected {
            destination: conn.destination.connect_id.as_str().into(),
            since: conn.phase.0,
            tunnel_ping_rtt: conn.tunnel_ping.rtt,
            stall: conn.tunnel_ping.failing_since.map(|since| ipc::TunnelStall {
                since,
                failed_pings: conn.tunnel_ping.failures,
                reconnect_at,
            }),
            pix,
        },
        _ => ipc::Connection::Disconnected,
    }
}

/// Best-effort telemetry for nerd stats; one scrape feeds both the strategy counters and the SURB gauges.
fn nerd_stats_telemetry() -> Option<String> {
    match hopr::telemetry() {
        Ok(t) => Some(t),
        Err(err) => {
            tracing::warn!(?err, "failed to collect hopr telemetry for nerd stats");
            None
        }
    }
}

/// The worker's fd must close before root's: on Linux a stale fd turns the next TUN create into a second queue on the old device.
async fn wait_for_pump_stop(pump_tasks: TaskTracker) -> bool {
    pump_tasks.close();
    let stopped = time::timeout(PUMP_STOP_BUDGET, pump_tasks.wait()).await.is_ok();
    if !stopped {
        tracing::warn!(
            ?PUMP_STOP_BUDGET,
            "wg pump did not stop in time - proceeding with tunnel teardown"
        );
    }
    stopped
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

fn connect_step(rh: &RouteHealth, probe: Option<&Probe>, dest: &Destination) -> ConnectStep {
    if rh.is_unrecoverable() {
        return ConnectStep::Refuse;
    }
    if !rh.is_routable() {
        return ConnectStep::WaitRoutable;
    }
    // Same exit is not enough: a probe opened before discovery moved it still talks to the old server.
    let ours = probe.filter(|p| p.key() == dest.key() && !probe_is_stale(Some(dest), p.destination()));
    match ours {
        Some(p) => match p.ready_session() {
            Some(session) => ConnectStep::Connect(session.clone()),
            None => ConnectStep::WaitProbe,
        },
        None => ConnectStep::StartProbe,
    }
}

/// A probe whose exit is gone, or whose endpoints moved under it, talks to a server that is no longer current.
fn probe_is_stale(current: Option<&Destination>, probed: &Destination) -> bool {
    current.is_none_or(|current| {
        current.gnosis_vpn_server != probed.gnosis_vpn_server || current.wireguard_server != probed.wireguard_server
    })
}

/// Whether closing this probe would pull its session out from under an exchange still running over it.
fn probe_session_busy(phase: &Phase, disconnecting: &[connection::down::Down], probed: &Destination) -> bool {
    let bridging =
        matches!(phase, Phase::Connecting(conn) | Phase::Connected(conn) if conn.destination.same_exit(probed));
    // Past ClosingBridge the down runner is done with the session; before it, closing the probe fails the unregister.
    let unregistering = disconnecting
        .iter()
        .any(|d| d.destination.same_exit(probed) && d.phase.1 != connection::down::Phase::ClosingBridge);
    bridging || unregistering
}

/// The probe's session still has a user, or a target is waiting on it; either way it must not be dropped.
fn probe_in_use(
    phase: &Phase,
    disconnecting: &[connection::down::Down],
    target: Option<ExitKey>,
    probed: &Destination,
) -> bool {
    let registering = matches!(phase, Phase::Connecting(conn) if conn.destination.same_exit(probed));
    let unregistering = disconnecting.iter().any(|d| d.destination.same_exit(probed));
    // Nothing restarts a probe for an already accepted target, so dropping it strands the connection.
    let awaited = matches!(phase, Phase::HoprRunning) && target == Some(probed.key());
    registering || unregistering || awaited
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

    /// A freshly initialised core: new identity in a temp state home, no node, no target, root on `from_core`.
    struct TestCore {
        core: Core,
        from_core: mpsc::Receiver<CoreToWorker>,
        results: mpsc::Sender<Results>,
        _state_home: tempfile::TempDir,
    }

    async fn fresh_core() -> TestCore {
        fresh_core_observable().await.0
    }

    /// Same as [`fresh_core`], but hands out the results receiver so a test can see scheduled ticks.
    async fn fresh_core_observable() -> (TestCore, mpsc::Receiver<Results>) {
        let state_home = tempfile::tempdir().expect("temp dir");
        let path = state_home.path().join("config.toml");
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let geoip_db = fixtures.join("geoip-country.mmdb");
        let toml = format!("version = 7\ngeoip_db = {geoip_db:?}\ncurvy_zk_dir = {fixtures:?}\n");
        tokio::fs::write(&path, toml).await.expect("write config");
        let config = crate::config::read(&path).await.expect("valid config");
        let worker_params = crate::worker_params::WorkerParams::new(
            crate::worker_params::ConfigFileMode::Manual(path),
            crate::worker_params::AllowFlags::default(),
            "https://blokli.invalid".parse().expect("valid url"),
            net::Ipv4Addr::new(203, 0, 113, 7),
            crate::dirs::user_dirs(state_home.path()),
            "test-net".to_string(),
        );
        // The worker creates this before Core::init; the generated pass file lands in it.
        worker_params.ensure_dirs().expect("state home layout");
        let (outgoing, from_core) = mpsc::channel(32);
        let (core, _) = Core::init(config, worker_params, None, outgoing)
            .await
            .expect("core init");
        let (results, results_receiver) = mpsc::channel(32);
        (
            TestCore {
                core,
                from_core,
                results,
                _state_home: state_home,
            },
            results_receiver,
        )
    }

    /// Install a routable destination the way discovery would, so `act_on_target` gets past its guards.
    fn with_destination(core: &mut Core, id: &str) -> Destination {
        let dest = destination(id);
        let mut rh = tracker(&dest);
        // Routable, as a completed graph walk would leave it, so `act_on_target` gets to the probe.
        rh.set_routable(true);
        core.route_healths.insert(dest.key(), rh);
        core.config.destinations.insert(dest.clone());
        dest
    }

    /// A real host-side connection failure, produced on its own unpaused runtime: a paused test clock
    /// would otherwise auto-advance into reqwest's timeout before the refused connect surfaces.
    fn host_side_failure() -> connection::up::Error {
        let err = std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(crate::gvpn_client::closed_listener_error())
        })
        .join()
        .expect("no panic in the helper thread");
        connection::up::Error::GvpnClient(err)
    }

    fn connected_with_key(id: &str) -> Up {
        let mut up = attempt(id, UpPhase::ConnectionEstablished);
        up.wireguard = Some(wireguard::WireGuard::new(
            wireguard::Config::new(None, None),
            wireguard::KeyPair {
                priv_key: "priv".into(),
                public_key: "pub".into(),
            },
        ));
        up
    }

    fn teardown_requested(from_core: &mut mpsc::Receiver<CoreToWorker>) -> bool {
        matches!(
            from_core.try_recv(),
            Ok(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
        )
    }

    #[tokio::test]
    async fn a_ping_failure_keeps_key_and_device() {
        let TestCore {
            mut core,
            mut from_core,
            results,
            ..
        } = fresh_core().await;
        core.phase = Phase::Connected(connected_with_key("exit"));

        core.reconnect_in_place(connected_with_key("exit"), true, &results)
            .await;

        assert!(!teardown_requested(&mut from_core));
        assert!(matches!(core.phase, Phase::HoprRunning));
        let resume = core.resume.as_ref().expect("key kept");
        assert_eq!(resume.wireguard.key_pair.public_key, "pub");
    }

    #[tokio::test]
    async fn a_wan_change_keeps_the_key_but_rebuilds_the_device() {
        let TestCore {
            mut core,
            mut from_core,
            results,
            ..
        } = fresh_core().await;

        core.reconnect_in_place(connected_with_key("exit"), false, &results)
            .await;

        assert!(teardown_requested(&mut from_core));
        assert_eq!(
            core.resume.as_ref().map(|r| r.wireguard.key_pair.public_key.as_str()),
            Some("pub")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pump_that_will_not_stop_forces_a_rebuild() {
        let TestCore {
            mut core,
            mut from_core,
            results,
            ..
        } = fresh_core().await;
        let stuck = TaskTracker::new();
        stuck.spawn(std::future::pending::<()>());
        core.wg_pump_tasks = stuck;

        core.reconnect_in_place(connected_with_key("exit"), true, &results)
            .await;

        assert!(teardown_requested(&mut from_core));
        assert!(core.resume.is_some());
    }

    #[tokio::test]
    async fn a_connection_without_a_key_reconnects_fresh() {
        let TestCore { mut core, results, .. } = fresh_core().await;

        core.reconnect_in_place(attempt("exit", UpPhase::GeneratingWg), true, &results)
            .await;

        assert!(core.resume.is_none());
    }

    #[tokio::test]
    async fn a_user_action_abandons_the_resume_and_tears_down() {
        let TestCore {
            mut core,
            mut from_core,
            results,
            ..
        } = fresh_core().await;
        core.resume = connection::up::Resume::try_from(&connected_with_key("exit")).ok();

        core.abandon_resume(&results).await;

        assert!(core.resume.is_none());
        assert!(teardown_requested(&mut from_core));
    }

    #[test]
    fn an_abandoned_resume_unregisters_its_key() {
        let resume = connection::up::Resume::try_from(&connected_with_key("exit")).expect("resumable");
        let down: connection::down::Down = resume.into();
        assert_eq!(down.wg_public_key, "pub");
        assert_eq!(down.destination.connect_id, destination("exit").connect_id);
    }

    #[tokio::test(start_paused = true)]
    async fn pump_stop_reports_a_pump_that_ignores_cancellation() {
        let stuck = TaskTracker::new();
        stuck.spawn(std::future::pending::<()>());
        assert!(!wait_for_pump_stop(stuck).await);

        assert!(wait_for_pump_stop(TaskTracker::new()).await);
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

    fn disconnection(dest: &Destination) -> connection::down::Down {
        connection::down::Down {
            destination: dest.clone(),
            phase: (SystemTime::UNIX_EPOCH, connection::down::Phase::UnregisterWg),
            wg_public_key: "key".to_string(),
        }
    }

    #[test]
    fn a_free_probe_may_be_dropped() {
        let probed = destination("exit");
        assert!(!probe_in_use(&Phase::HoprRunning, &[], None, &probed));
    }

    #[test]
    fn a_probe_being_registered_or_unregistered_over_is_in_use() {
        let probed = destination("exit");
        let connecting = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
        assert!(probe_in_use(&connecting, &[], None, &probed));
        assert!(probe_in_use(
            &Phase::HoprRunning,
            &[disconnection(&probed)],
            None,
            &probed
        ));
    }

    #[test]
    fn a_probe_an_accepted_target_waits_for_is_in_use() {
        let probed = destination("exit");
        assert!(probe_in_use(&Phase::HoprRunning, &[], Some(probed.key()), &probed));
    }

    #[test]
    fn a_connected_target_no_longer_holds_its_probe() {
        let probed = destination("exit");
        let connected = Phase::Connected(attempt("exit", UpPhase::RegisterWg));
        assert!(!probe_in_use(&connected, &[], Some(probed.key()), &probed));
    }

    #[test]
    fn a_probe_a_disconnect_still_unregisters_over_is_busy() {
        let probed = destination("exit");
        assert!(probe_session_busy(
            &Phase::HoprRunning,
            &[disconnection(&probed)],
            &probed
        ));
        assert!(!probe_session_busy(&Phase::HoprRunning, &[], &probed));
        let mut other = destination("other");
        other.address = Address::from([2u8; 20]);
        assert!(!probe_session_busy(
            &Phase::HoprRunning,
            &[disconnection(&other)],
            &probed
        ));
    }

    #[test]
    fn a_disconnect_past_its_unregister_frees_the_probe() {
        let probed = destination("exit");
        let mut closing = disconnection(&probed);
        closing.disconnect_evt(connection::down::Event::CloseBridge);
        assert!(!probe_session_busy(&Phase::HoprRunning, &[closing], &probed));
    }

    #[test]
    fn a_connection_bridging_over_its_probe_keeps_it_busy() {
        let probed = destination("exit");
        let connected = Phase::Connected(attempt("exit", UpPhase::RegisterWg));
        assert!(probe_session_busy(&connected, &[], &probed));
        let connecting = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
        assert!(probe_session_busy(&connecting, &[], &probed));
    }

    #[test]
    fn a_probe_is_stale_once_its_exit_is_gone_or_has_moved() {
        let probed = destination("exit");
        assert!(probe_is_stale(None, &probed));
        assert!(!probe_is_stale(Some(&probed), &probed));

        let mut moved = probed.clone();
        moved.gnosis_vpn_server = "172.30.0.2:8000".parse().expect("valid socket address");
        assert!(probe_is_stale(Some(&moved), &probed));
    }

    /// The host-side budget decides between rebuilding the tunnel, rotating the worker, and nothing.
    #[test]
    fn a_connection_failure_is_routed_by_fault_and_budget() {
        // Not the target's attempt any more: a replacement connection is already running.
        assert_eq!(connection_failure_action(false, true, 0), FailureAction::Ignore);
        assert_eq!(connection_failure_action(false, false, 0), FailureAction::Ignore);
        // Host-side failures rebuild the tunnel while the budget lasts ...
        assert_eq!(connection_failure_action(true, true, 0), FailureAction::RetryHostSide);
        assert_eq!(
            connection_failure_action(true, true, HOST_SIDE_FAILURE_RETRIES - 1),
            FailureAction::RetryHostSide
        );
        // ... and only then rotate the worker, which is the escape hatch.
        assert_eq!(
            connection_failure_action(true, true, HOST_SIDE_FAILURE_RETRIES),
            FailureAction::RestartWorker
        );
        // A failure the exit caused is never retried in place: the worker holds the node state.
        assert_eq!(connection_failure_action(true, false, 0), FailureAction::RestartWorker);
    }

    #[test]
    fn host_side_retries_back_off_and_stay_bounded() {
        // One delay per retry the budget allows; the last one is the cap.
        let delays: Vec<Duration> = (1..=HOST_SIDE_FAILURE_RETRIES).map(host_side_retry_delay).collect();
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]), "{delays:?}");
        assert_eq!(delays[0], Duration::from_secs(2));
        assert_eq!(delays.last().copied(), Some(Duration::from_secs(10)), "{delays:?}");
    }

    /// A host-side failure must rebuild the tunnel and wait, not rotate the worker - and must not blame
    /// the exit's route for a local problem.
    #[tokio::test]
    async fn a_host_side_failure_rebuilds_the_tunnel_and_cools_off() {
        let TestCore {
            mut core,
            mut from_core,
            results,
            ..
        } = fresh_core().await;
        core.target = Some("exit".to_string());
        core.phase = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
        let host_side = host_side_failure();

        let keep_running = core
            .on_results(Results::ConnectionResult { res: Err(host_side) }, &results)
            .await;

        assert!(keep_running, "the worker must survive a host-side failure");
        assert!(teardown_requested(&mut from_core), "the dead device must be torn down");
        assert!(matches!(core.phase, Phase::HoprRunning));
        assert_eq!(core.host_side_failures, 1);
        assert!(core.retry_not_before.is_some(), "the retry must wait for the cool-off");
        assert!(core.reconnecting_since.is_some(), "status keeps reporting a reconnect");
    }

    /// Once the budget is spent the worker is rotated: that stays the escape hatch.
    #[tokio::test]
    async fn a_host_side_failure_past_the_budget_restarts_the_worker() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        core.target = Some("exit".to_string());
        core.phase = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
        core.host_side_failures = HOST_SIDE_FAILURE_RETRIES;
        let host_side = host_side_failure();

        let keep_running = core
            .on_results(Results::ConnectionResult { res: Err(host_side) }, &results)
            .await;

        assert!(!keep_running, "the worker rotation is the last resort");
    }

    /// The cool-off gate defers a reconnect that another trigger would otherwise start.
    #[tokio::test]
    async fn the_cool_off_holds_back_reconnects_until_it_elapsed() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::HoprRunning;
        core.retry_not_before = Some(time::Instant::now() + Duration::from_secs(60));

        // Without the gate this starts a probe at once; routability/probe events land here too.
        core.act_on_target(&results);

        assert!(core.probe.is_none(), "still waiting for the cool-off");
        assert!(core.retry_not_before.is_some(), "the gate stays armed");

        // Once the deadline passed, the very next drive gets through and spends the gate.
        core.retry_not_before = Some(time::Instant::now());
        core.act_on_target(&results);

        assert!(
            core.retry_not_before.is_none(),
            "the attempt starts once the cool-off elapsed"
        );
    }

    /// The host-side failure schedules the retry tick; the tick is what ends the cool-off, and a
    /// newer failure supersedes an older tick.
    #[tokio::test(start_paused = true)]
    async fn a_host_side_failure_schedules_exactly_one_retry_tick() {
        let (TestCore { mut core, results, .. }, mut ticks) = fresh_core_observable().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());

        // Two failures in a row: the second supersedes the first tick and its shorter cool-off.
        for _ in 0..2 {
            core.phase = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
            let host_side = host_side_failure();
            assert!(
                core.on_results(Results::ConnectionResult { res: Err(host_side) }, &results)
                    .await,
                "the worker survives host-side failures"
            );
        }
        let deadline = time::Instant::now() + host_side_retry_delay(2);
        assert_eq!(
            core.retry_not_before,
            Some(deadline),
            "the gate and the timer share one deadline"
        );

        // The superseded 2 s tick must not fire. A bounded wait, not `try_recv`: delivery needs a
        // poll, and `advance` only yields once.
        time::advance(Duration::from_secs(4)).await;
        assert!(
            time::timeout(Duration::from_millis(100), ticks.recv()).await.is_err(),
            "the superseded tick must be gone"
        );

        // Its own deadline does.
        time::advance(Duration::from_secs(2)).await;
        let tick = time::timeout(Duration::from_secs(1), ticks.recv())
            .await
            .expect("a tick at the second deadline")
            .expect("the tick channel stays open");
        assert!(
            matches!(tick, Results::RetryConnection { not_before } if not_before == deadline),
            "unexpected tick: {tick}"
        );
        assert!(ticks.try_recv().is_err(), "exactly one tick");
    }

    /// A tick that was already queued when a newer cool-off was armed must not end that cool-off.
    #[tokio::test(start_paused = true)]
    async fn a_superseded_tick_leaves_a_newer_cool_off_alone() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::HoprRunning;
        let stale = time::Instant::now();
        let newer = stale + Duration::from_secs(5);
        core.retry_not_before = Some(newer);

        assert!(
            core.on_results(Results::RetryConnection { not_before: stale }, &results)
                .await
        );

        assert_eq!(core.retry_not_before, Some(newer), "the newer deadline survives");
        assert!(core.probe.is_none(), "and nothing reconnected early");
    }

    /// Only a waiting target in `HoprRunning` may be re-driven by a tick.
    #[test]
    fn a_retry_tick_is_only_due_for_a_waiting_target() {
        assert!(retry_is_due(Some("exit"), &Phase::HoprRunning));
        assert!(!retry_is_due(None, &Phase::HoprRunning));
        assert!(!retry_is_due(
            Some("exit"),
            &Phase::Connecting(attempt("exit", UpPhase::RegisterWg))
        ));
        assert!(!retry_is_due(
            Some("exit"),
            &Phase::Connected(connected_with_key("exit"))
        ));
    }

    /// A host-side failure must not be recorded against the exit's route: the exit never saw it.
    #[tokio::test]
    async fn a_host_side_failure_leaves_the_route_health_alone() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));

        assert!(
            core.on_results(
                Results::ConnectionResult {
                    res: Err(host_side_failure())
                },
                &results
            )
            .await
        );

        assert!(
            core.route_healths
                .get(&dest.key())
                .expect("tracker")
                .last_error()
                .is_none(),
            "a local failure is not the exit's fault"
        );
    }

    /// A disconnect during the cool-off must take the pending tick with it.
    #[tokio::test(start_paused = true)]
    async fn a_disconnect_drops_the_pending_retry() {
        let (TestCore { mut core, results, .. }, mut ticks) = fresh_core_observable().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::Connecting(attempt("exit", UpPhase::RegisterWg));
        assert!(
            core.on_results(
                Results::ConnectionResult {
                    res: Err(host_side_failure())
                },
                &results
            )
            .await
        );

        let (resp, _rx) = oneshot::channel();
        assert!(
            core.on_event(
                WorkerToCore::WorkerCommand {
                    cmd: WorkerCommand::Disconnect,
                    resp,
                },
                &results,
            )
            .await
        );

        assert!(core.retry_not_before.is_none(), "no gate left behind");
        time::advance(Duration::from_secs(30)).await;
        assert!(
            time::timeout(Duration::from_millis(100), ticks.recv()).await.is_err(),
            "the tick was cancelled"
        );
    }

    /// An attempt that already waits for its probe is still pending: a re-Connect keeps its key.
    #[tokio::test]
    async fn a_repeated_connect_while_waiting_for_the_probe_keeps_the_key() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::HoprRunning;
        // Waiting for the probe: no gate, but the key of the attempt is kept.
        core.resume =
            Some(connection::up::Resume::try_from(&connected_with_key("exit")).expect("a key to resume with"));

        let (resp, rx) = oneshot::channel();
        assert!(
            core.on_event(
                WorkerToCore::WorkerCommand {
                    cmd: WorkerCommand::Connect {
                        destination: dest.connect_id.clone().into()
                    },
                    resp,
                },
                &results,
            )
            .await
        );

        assert!(core.resume.is_some(), "the kept key survives");
        assert!(matches!(
            rx.await.expect("reply"),
            Response::Connect(ipc::ConnectResponse::Connecting { .. })
        ));
    }

    /// A Connect that is not accepted leaves the pending target's kept key alone.
    #[tokio::test]
    async fn a_rejected_connect_keeps_the_pending_targets_key() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        // The pending target is an exit discovery no longer publishes.
        core.target = Some("gone".to_string());
        core.phase = Phase::HoprRunning;
        core.resume =
            Some(connection::up::Resume::try_from(&connected_with_key("gone")).expect("a key to resume with"));
        core.host_side_failures = 2;

        // Unrecoverable is a refusal; not-routable is only a wait, and a wait is accepted.
        let refused = with_destination(&mut core, "other");
        core.route_healths
            .get_mut(&refused.key())
            .expect("tracker")
            .set_cannot_open_session("no session".to_string());

        let (resp, rx) = oneshot::channel();
        assert!(
            core.on_event(
                WorkerToCore::WorkerCommand {
                    cmd: WorkerCommand::Connect {
                        destination: refused.connect_id.clone().into()
                    },
                    resp,
                },
                &results,
            )
            .await
        );

        assert_eq!(core.target.as_deref(), Some("gone"), "the pending target stays");
        assert_eq!(core.host_side_failures, 2, "and its budget");
        assert!(core.resume.is_some(), "and its kept key");
        assert!(matches!(
            rx.await.expect("reply"),
            Response::Connect(ipc::ConnectResponse::UnableToConnect { .. })
        ));
    }

    /// A Connect that only waits for its route must not cancel the connection it waits behind.
    #[tokio::test]
    async fn a_waiting_connect_keeps_the_live_connection_running() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let active = with_destination(&mut core, "active");
        core.target = Some(active.connect_id.clone());
        core.phase = Phase::Connected(connected_with_key("active"));

        // A different exit, not another entry for the active one: the helper's fixed address would
        // make `same_exit` true.
        let mut waiting = destination("waiting");
        waiting.address = Address::from([2u8; 20]);
        let mut waiting_health = tracker(&waiting);
        waiting_health.set_routable(false);
        core.route_healths.insert(waiting.key(), waiting_health);
        core.config.destinations.insert(waiting.clone());

        let (resp, rx) = oneshot::channel();
        assert!(
            core.on_event(
                WorkerToCore::WorkerCommand {
                    cmd: WorkerCommand::Connect {
                        destination: waiting.connect_id.clone().into()
                    },
                    resp,
                },
                &results,
            )
            .await
        );

        assert!(matches!(
            rx.await.expect("reply"),
            Response::Connect(ipc::ConnectResponse::WaitingToConnect { .. })
        ));
        assert_eq!(core.target.as_deref(), Some("waiting"), "the new target is pending");
        assert!(
            !core.cancel_connection.is_cancelled(),
            "the connection it waits behind keeps running"
        );
    }

    /// A repeated Connect to the cooling-off exit answers "connecting": it keeps the waiting
    /// attempt's budget, its cool-off and the WireGuard key it will resume with.
    #[tokio::test]
    async fn a_repeated_connect_during_the_cool_off_keeps_budget_gate_and_key() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        let dest = with_destination(&mut core, "exit");
        core.target = Some(dest.connect_id.clone());
        core.phase = Phase::Connecting(connected_with_key("exit"));
        assert!(
            core.on_results(
                Results::ConnectionResult {
                    res: Err(host_side_failure())
                },
                &results
            )
            .await
        );
        let gate = core.retry_not_before.expect("cooling off");
        let budget = core.host_side_failures;
        assert!(core.resume.is_some(), "the key is kept for the retry");

        let (resp, rx) = oneshot::channel();
        assert!(
            core.on_event(
                WorkerToCore::WorkerCommand {
                    cmd: WorkerCommand::Connect {
                        destination: dest.connect_id.clone().into()
                    },
                    resp,
                },
                &results,
            )
            .await
        );

        assert_eq!(core.host_side_failures, budget, "no refilled budget");
        assert_eq!(core.retry_not_before, Some(gate), "the cool-off still runs");
        assert!(core.resume.is_some(), "the kept key survives");
        assert!(
            matches!(
                rx.await.expect("reply"),
                Response::Connect(ipc::ConnectResponse::Connecting { .. })
            ),
            "reported as still connecting"
        );
    }

    /// A retry tick that lands after the user disconnected must not reconnect.
    #[tokio::test]
    async fn a_retry_tick_without_a_target_is_a_no_op() {
        let TestCore { mut core, results, .. } = fresh_core().await;
        with_destination(&mut core, "exit");
        core.phase = Phase::HoprRunning;
        core.target = None;

        assert!(
            core.on_results(
                Results::RetryConnection {
                    not_before: time::Instant::now()
                },
                &results
            )
            .await
        );

        assert!(matches!(core.phase, Phase::HoprRunning));
        assert!(core.probe.is_none(), "nothing to connect to, nothing to probe");
    }

    #[test]
    fn connect_step_refuses_a_latched_route() {
        let dest = latched_destination("exit");
        assert_eq!(connect_step(&tracker(&dest), None, &dest), ConnectStep::Refuse);
    }

    #[test]
    fn connect_step_waits_for_a_route_before_probing() {
        let dest = destination("exit");
        assert_eq!(connect_step(&tracker(&dest), None, &dest), ConnectStep::WaitRoutable);
    }

    #[test]
    fn connect_step_starts_a_probe_once_routable() {
        let dest = destination("exit");
        let mut rh = tracker(&dest);
        rh.set_routable(true);
        assert_eq!(connect_step(&rh, None, &dest), ConnectStep::StartProbe);
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

    fn view(phase: &Phase, reconnecting_since: Option<SystemTime>, target: Option<&str>) -> ipc::Connection {
        connection_view(phase, reconnecting_since, target, 3, None)
    }

    #[test]
    fn waiting_on_route_health_reports_a_reconnect_without_a_phase() {
        let since = SystemTime::UNIX_EPOCH;
        assert_eq!(
            view(&Phase::HoprRunning, Some(since), Some("exit")),
            ipc::Connection::Reconnecting {
                destination: "exit".into(),
                since,
                phase: None,
            }
        );
    }

    #[test]
    fn a_first_connect_waiting_on_route_health_is_not_a_reconnect() {
        assert_eq!(
            view(&Phase::HoprRunning, None, Some("exit")),
            ipc::Connection::Disconnected
        );
    }

    // After a worker restart the target can name a discovered exit the config has never seen.
    #[test]
    fn a_target_not_yet_published_by_discovery_is_still_reported() {
        let connection = view(&Phase::HoprRunning, Some(SystemTime::UNIX_EPOCH), Some("0xdiscovered"));
        assert!(matches!(connection, ipc::Connection::Reconnecting { .. }));
        assert_eq!(connection.destination(), Some(&"0xdiscovered".into()));
    }

    #[test]
    fn a_cleared_target_reports_nothing() {
        assert_eq!(
            view(&Phase::HoprRunning, Some(SystemTime::UNIX_EPOCH), None),
            ipc::Connection::Disconnected
        );
    }

    #[test]
    fn an_attempt_in_flight_reports_its_phase() {
        let phase = Phase::Connecting(attempt("exit", UpPhase::VerifyPing));
        assert_eq!(
            view(&phase, Some(SystemTime::UNIX_EPOCH), Some("exit")),
            ipc::Connection::Reconnecting {
                destination: "exit".into(),
                since: SystemTime::UNIX_EPOCH,
                phase: Some(UpPhase::VerifyPing),
            }
        );
    }

    // Guards the system tests, which poll for `connecting` to follow a fresh connection.
    #[test]
    fn a_fresh_connection_still_reports_connecting() {
        let phase = Phase::Connecting(attempt("exit", UpPhase::VerifyPing));
        let connection = view(&phase, None, None);
        assert!(
            matches!(connection, ipc::Connection::Connecting { ref phase, .. } if *phase == UpPhase::VerifyPing),
            "{connection:?}"
        );
        assert_eq!(connection.destination(), Some(&"exit".into()));
    }

    #[test]
    fn a_live_connection_never_reports_a_reconnect() {
        let phase = Phase::Connected(attempt("exit", UpPhase::ConnectionEstablished));
        let connection = view(&phase, Some(SystemTime::UNIX_EPOCH), Some("exit"));
        assert!(
            matches!(connection, ipc::Connection::Connected { .. }),
            "{connection:?}"
        );
    }
}
