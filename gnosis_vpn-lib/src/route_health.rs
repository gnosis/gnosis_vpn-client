/// Health model per destination route.
/// Tracks both network reachability (peers, channels) and exit node health (via TCP sessions).
use edgli::hopr_lib::SessionClientConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::collections::HashSet;
use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};
use crate::connection::options::Options;
use crate::core::runner::Results;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::{gvpn_client, log_output};

const MAX_INTERVAL_BETWEEN_FAILURES: Duration = Duration::from_mins(5);
const FAILURE_INTERVAL: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChannelNeed {
    Channel(Address),
    AnyChannel,
    Peering(Address),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UnrecoverableReason {
    NotAllowed,
    InvalidId,
    InvalidPath,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub enum ExitHealth {
    #[default]
    Init,
    Checking {
        since: SystemTime,
    },
    Healthy {
        checked_at: SystemTime,
        version: Option<gvpn_client::Versions>,
        ping_rtt: Duration,
        health: Option<gvpn_client::Health>,
        total_time: Duration,
    },
    Unhealthy {
        checked_at: SystemTime,
        error: String,
        previous_failures: u32,
    },
}

/// Serializable state — sent to CLI via command API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RouteHealthState {
    Unrecoverable {
        id: String,
        reason: UnrecoverableReason,
    },
    NeedsPeering {
        id: String,
        need: ChannelNeed,
        funded: bool,
        exit: ExitHealth,
        last_error: Option<String>,
    },
    NeedsFunding {
        id: String,
        need: ChannelNeed,
        exit: ExitHealth,
        last_error: Option<String>,
    },
    /// Peer and channel requirements are met. Health checks are running.
    Routable {
        id: String,
        need: ChannelNeed,
        exit: ExitHealth,
        last_error: Option<String>,
    },
    /// Exit health confirmed healthy. Safe to connect.
    ReadyToConnect {
        id: String,
        need: ChannelNeed,
        exit: ExitHealth,
        last_error: Option<String>,
    },
}

/// Returned from `peers()` so Core knows what side effects to trigger.
#[derive(Debug, PartialEq)]
pub enum PeerTransition {
    NoChange,
    /// Core should spawn channel funding.
    NowNeedsFunding,
    /// Route became routable. Health check spawned internally.
    BecameRoutable,
    /// Peer lost. Health check cancelled internally.
    LostPeer,
}

/// Full runtime type — owns the health check lifecycle.
pub struct RouteHealth {
    state: RouteHealthState,
    health_check_cancel: CancellationToken,
    cancel_on_shutdown: CancellationToken,
    check_cycle: u32,
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl RouteHealth {
    pub fn new(dest: &Destination, allow_insecure: bool, cancel_on_shutdown: CancellationToken) -> Self {
        let state = match dest.routing.clone() {
            RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 => {
                if allow_insecure {
                    RouteHealthState::NeedsPeering {
                        id: dest.id.clone(),
                        need: ChannelNeed::Peering(dest.address),
                        funded: false,
                        exit: ExitHealth::Init,
                        last_error: None,
                    }
                } else {
                    RouteHealthState::Unrecoverable {
                        id: dest.id.clone(),
                        reason: UnrecoverableReason::NotAllowed,
                    }
                }
            }
            RoutingOptions::Hops(_) => RouteHealthState::NeedsPeering {
                id: dest.id.clone(),
                need: ChannelNeed::AnyChannel,
                funded: false,
                exit: ExitHealth::Init,
                last_error: None,
            },
            RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
                Some(NodeId::Chain(address)) => RouteHealthState::NeedsPeering {
                    id: dest.id.clone(),
                    need: ChannelNeed::Channel(address),
                    funded: false,
                    exit: ExitHealth::Init,
                    last_error: None,
                },
                Some(NodeId::Offchain(_)) => RouteHealthState::Unrecoverable {
                    id: dest.id.clone(),
                    reason: UnrecoverableReason::InvalidId,
                },
                None => RouteHealthState::Unrecoverable {
                    id: dest.id.clone(),
                    reason: UnrecoverableReason::InvalidPath,
                },
            },
        };
        Self {
            state,
            health_check_cancel: CancellationToken::new(),
            cancel_on_shutdown,
            check_cycle: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

impl RouteHealth {
    pub fn id(&self) -> &str {
        match &self.state {
            RouteHealthState::Unrecoverable { id, .. }
            | RouteHealthState::NeedsPeering { id, .. }
            | RouteHealthState::NeedsFunding { id, .. }
            | RouteHealthState::Routable { id, .. }
            | RouteHealthState::ReadyToConnect { id, .. } => id,
        }
    }

    pub fn state(&self) -> &RouteHealthState {
        &self.state
    }

    pub fn needs_peer(&self) -> bool {
        matches!(self.state, RouteHealthState::NeedsPeering { .. })
    }

    pub fn needs_channel_funding(&self) -> Option<Address> {
        match &self.state {
            RouteHealthState::NeedsFunding {
                need: ChannelNeed::Channel(addr),
                ..
            } => Some(*addr),
            _ => None,
        }
    }

    pub fn needs_any_channel_funding(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::NeedsFunding {
                need: ChannelNeed::AnyChannel,
                ..
            }
        )
    }

    pub fn is_routable(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::Routable { .. } | RouteHealthState::ReadyToConnect { .. }
        )
    }

    pub fn is_ready_to_connect(&self) -> bool {
        matches!(self.state, RouteHealthState::ReadyToConnect { .. })
    }

    pub fn is_unrecoverable(&self) -> bool {
        matches!(self.state, RouteHealthState::Unrecoverable { .. })
    }
}

// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

impl RouteHealth {
    pub fn peers(
        &mut self,
        addresses: &HashSet<Address>,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) -> PeerTransition {
        // Determine the transition from immutable state inspection
        enum Action {
            NoChange,
            BecameRoutable {
                id: String,
                need: ChannelNeed,
                exit: ExitHealth,
                last_error: Option<String>,
            },
            NowNeedsFunding {
                id: String,
                need: ChannelNeed,
                exit: ExitHealth,
                last_error: Option<String>,
            },
            LostPeerFromFunding {
                id: String,
                need: ChannelNeed,
                exit: ExitHealth,
                last_error: Option<String>,
            },
            LostPeer {
                id: String,
                need: ChannelNeed,
                funded: bool,
                exit: ExitHealth,
                last_error: Option<String>,
            },
        }

        let action = match &self.state {
            RouteHealthState::NeedsPeering {
                id,
                need,
                funded,
                exit,
                last_error,
            } => {
                let is_peered = match need {
                    ChannelNeed::Channel(addr) => addresses.contains(addr),
                    ChannelNeed::AnyChannel => !addresses.is_empty(),
                    ChannelNeed::Peering(addr) => addresses.contains(addr),
                };
                if !is_peered {
                    Action::NoChange
                } else if *funded || matches!(need, ChannelNeed::Peering(_)) {
                    Action::BecameRoutable {
                        id: id.clone(),
                        need: need.clone(),
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    }
                } else {
                    Action::NowNeedsFunding {
                        id: id.clone(),
                        need: need.clone(),
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    }
                }
            }
            RouteHealthState::NeedsFunding {
                id,
                need,
                exit,
                last_error,
            } => {
                let still_peered = match need {
                    ChannelNeed::Channel(addr) => addresses.contains(addr),
                    ChannelNeed::AnyChannel => !addresses.is_empty(),
                    ChannelNeed::Peering(_) => unreachable!("Peering never enters NeedsFunding"),
                };
                if still_peered {
                    Action::NoChange
                } else {
                    Action::LostPeerFromFunding {
                        id: id.clone(),
                        need: need.clone(),
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    }
                }
            }
            RouteHealthState::Routable {
                id,
                need,
                exit,
                last_error,
            }
            | RouteHealthState::ReadyToConnect {
                id,
                need,
                exit,
                last_error,
            } => {
                let still_peered = match need {
                    ChannelNeed::Channel(addr) => addresses.contains(addr),
                    ChannelNeed::AnyChannel => !addresses.is_empty(),
                    ChannelNeed::Peering(addr) => addresses.contains(addr),
                };
                if still_peered {
                    Action::NoChange
                } else {
                    let funded = !matches!(need, ChannelNeed::Peering(_));
                    Action::LostPeer {
                        id: id.clone(),
                        need: need.clone(),
                        funded,
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    }
                }
            }
            RouteHealthState::Unrecoverable { .. } => Action::NoChange,
        };

        // Apply mutations after the immutable borrow is released
        match action {
            Action::NoChange => PeerTransition::NoChange,
            Action::BecameRoutable {
                id,
                need,
                exit,
                last_error,
            } => {
                self.state = RouteHealthState::Routable {
                    id,
                    need,
                    exit,
                    last_error,
                };
                self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
                PeerTransition::BecameRoutable
            }
            Action::NowNeedsFunding {
                id,
                need,
                exit,
                last_error,
            } => {
                self.state = RouteHealthState::NeedsFunding {
                    id,
                    need,
                    exit,
                    last_error,
                };
                PeerTransition::NowNeedsFunding
            }
            Action::LostPeerFromFunding {
                id,
                need,
                exit,
                last_error,
            } => {
                self.state = RouteHealthState::NeedsPeering {
                    id,
                    need,
                    funded: false,
                    exit,
                    last_error,
                };
                PeerTransition::LostPeer
            }
            Action::LostPeer {
                id,
                need,
                funded,
                exit,
                last_error,
            } => {
                self.cancel_health_check();
                self.state = RouteHealthState::NeedsPeering {
                    id,
                    need,
                    funded,
                    exit,
                    last_error,
                };
                PeerTransition::LostPeer
            }
        }
    }

    pub fn channel_funded(
        &mut self,
        address: Address,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        let should_advance = match &self.state {
            RouteHealthState::NeedsFunding { need, .. } => match need {
                ChannelNeed::Channel(addr) => *addr == address,
                ChannelNeed::AnyChannel => true,
                ChannelNeed::Peering(_) => false,
            },
            _ => false,
        };
        if should_advance
            && let RouteHealthState::NeedsFunding {
                id,
                need,
                exit,
                last_error,
            } = self.state.clone()
        {
            self.state = RouteHealthState::Routable {
                id,
                need,
                exit,
                last_error,
            };
            self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
        }
    }

    pub fn health_check_result(
        &mut self,
        new_exit: ExitHealth,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        // Only process results when in Routable or ReadyToConnect
        let merged = match &self.state {
            RouteHealthState::Routable { exit: old_exit, .. }
            | RouteHealthState::ReadyToConnect { exit: old_exit, .. } => match (old_exit, &new_exit) {
                (ExitHealth::Unhealthy { previous_failures, .. }, ExitHealth::Unhealthy { checked_at, error, .. }) => {
                    Some(ExitHealth::Unhealthy {
                        checked_at: *checked_at,
                        error: error.clone(),
                        previous_failures: previous_failures + 1,
                    })
                }
                _ => Some(new_exit),
            },
            _ => None,
        };

        if let Some(merged) = merged {
            let ping_interval = options.health_check_intervals.ping;
            let next_interval = merged.next_interval(ping_interval);
            let is_healthy = matches!(merged, ExitHealth::Healthy { .. });

            self.set_exit(merged);

            // Transition between Routable ↔ ReadyToConnect based on exit health
            match &self.state {
                RouteHealthState::Routable {
                    id,
                    need,
                    exit,
                    last_error,
                } if is_healthy => {
                    self.state = RouteHealthState::ReadyToConnect {
                        id: id.clone(),
                        need: need.clone(),
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    };
                }
                RouteHealthState::ReadyToConnect {
                    id,
                    need,
                    exit,
                    last_error,
                } if !is_healthy => {
                    self.state = RouteHealthState::Routable {
                        id: id.clone(),
                        need: need.clone(),
                        exit: exit.clone(),
                        last_error: last_error.clone(),
                    };
                }
                _ => {}
            }

            if let Some(delay) = next_interval {
                self.spawn_health_check(delay, hopr, dest, options, sender);
            }
        }
    }

    pub fn with_error(&mut self, err: String) {
        match &mut self.state {
            RouteHealthState::NeedsPeering { last_error, .. }
            | RouteHealthState::NeedsFunding { last_error, .. }
            | RouteHealthState::Routable { last_error, .. }
            | RouteHealthState::ReadyToConnect { last_error, .. } => {
                *last_error = Some(err);
            }
            RouteHealthState::Unrecoverable { .. } => {}
        }
    }

    pub fn no_error(&mut self) {
        match &mut self.state {
            RouteHealthState::NeedsPeering { last_error, .. }
            | RouteHealthState::NeedsFunding { last_error, .. }
            | RouteHealthState::Routable { last_error, .. }
            | RouteHealthState::ReadyToConnect { last_error, .. } => {
                *last_error = None;
            }
            RouteHealthState::Unrecoverable { .. } => {}
        }
    }

    fn set_exit(&mut self, new_exit: ExitHealth) {
        match &mut self.state {
            RouteHealthState::NeedsPeering { exit, .. }
            | RouteHealthState::NeedsFunding { exit, .. }
            | RouteHealthState::Routable { exit, .. }
            | RouteHealthState::ReadyToConnect { exit, .. } => {
                *exit = new_exit;
            }
            RouteHealthState::Unrecoverable { .. } => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Health check spawn / cancel
// ---------------------------------------------------------------------------

/// Which checks to include in a health check cycle.
#[derive(Clone, Debug)]
struct CheckScope {
    version: bool,
    health: bool,
}

impl RouteHealth {
    pub fn spawn_health_check(
        &mut self,
        delay: Duration,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        self.cancel_health_check();

        let intervals = &options.health_check_intervals;
        let cycle = self.check_cycle;
        self.check_cycle = cycle.wrapping_add(1);

        let scope = CheckScope {
            version: cycle.is_multiple_of(intervals.version_every_n_pings),
            health: cycle.is_multiple_of(intervals.health_every_n_pings),
        };

        let token = self.health_check_cancel.clone();
        let shutdown = self.cancel_on_shutdown.clone();
        let hopr = hopr.clone();
        let dest = dest.clone();
        let options = options.clone();
        let sender = sender.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = shutdown.cancelled() => {}
                _ = async {
                    time::sleep(delay).await;
                    run_health_check(&hopr, &dest, &options, &scope, &sender).await;
                } => {}
            }
        });
    }

    pub fn cancel_health_check(&mut self) {
        self.health_check_cancel.cancel();
        self.health_check_cancel = CancellationToken::new();
    }
}

// ---------------------------------------------------------------------------
// Health check runner (async, runs in spawned task)
// ---------------------------------------------------------------------------

async fn run_health_check(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    scope: &CheckScope,
    sender: &mpsc::Sender<Results>,
) {
    let id = destination.id.clone();

    let _ = sender
        .send(Results::HealthCheck {
            id: id.clone(),
            exit: ExitHealth::Checking {
                since: SystemTime::now(),
            },
        })
        .await;

    let checked_at = SystemTime::now();
    let measure_total = Instant::now();

    let res_session = open_health_session(hopr, destination, options).await;
    let session = match res_session {
        Ok(session) => session,
        Err(err) => {
            let _ = sender
                .send(Results::HealthCheck {
                    id,
                    exit: ExitHealth::Unhealthy {
                        checked_at,
                        error: format!("Session creation error: {err}"),
                        previous_failures: 0,
                    },
                })
                .await;
            return;
        }
    };

    let socket_addr = session.bound_host;
    let timeout = options.timeouts.http;
    let client = reqwest::Client::new();

    // Step 1: Version check (when due)
    let mut version = None;
    if scope.version {
        match gvpn_client::versions(&client, socket_addr, timeout).await {
            Ok(v) => {
                tracing::debug!(%destination, versions = %v, "exit server version check passed");
                version = Some(v);
            }
            Err(err) => {
                close_health_session(hopr, &session).await;
                let _ = sender
                    .send(Results::HealthCheck {
                        id,
                        exit: ExitHealth::Unhealthy {
                            checked_at,
                            error: format!("Version check error: {err}"),
                            previous_failures: 0,
                        },
                    })
                    .await;
                return;
            }
        }
    }

    // Step 2: Ping (always runs)
    let measure_rtt = Instant::now();
    if let Err(err) = gvpn_client::ping(&client, socket_addr, timeout).await {
        close_health_session(hopr, &session).await;
        let _ = sender
            .send(Results::HealthCheck {
                id,
                exit: ExitHealth::Unhealthy {
                    checked_at,
                    error: format!("Ping error: {err}"),
                    previous_failures: 0,
                },
            })
            .await;
        return;
    }
    let ping_rtt = measure_rtt.elapsed();

    // Step 3: Exit health (when due)
    let mut health = None;
    if scope.health {
        match request_health(options, &session).await {
            Ok(h) => {
                tracing::debug!(%destination, health = %h, "exit health check passed");
                health = Some(h);
            }
            Err(err) => {
                close_health_session(hopr, &session).await;
                let _ = sender
                    .send(Results::HealthCheck {
                        id,
                        exit: ExitHealth::Unhealthy {
                            checked_at,
                            error: format!("Health request error: {err}"),
                            previous_failures: 0,
                        },
                    })
                    .await;
                return;
            }
        }
    }

    close_health_session(hopr, &session).await;
    let total_time = measure_total.elapsed();

    let new_exit = ExitHealth::Healthy {
        checked_at,
        version,
        ping_rtt,
        health,
        total_time,
    };

    let _ = sender.send(Results::HealthCheck { id, exit: new_exit }).await;
}

async fn open_health_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.health.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: None,
        ..Default::default()
    };
    tracing::debug!(%destination, "opening TCP session for health check");
    hopr.open_session(
        destination.address,
        options.sessions.health.target.clone(),
        Some(1),
        Some(1),
        cfg,
    )
    .await
}

async fn request_health(
    options: &Options,
    session: &SessionClientMetadata,
) -> Result<gvpn_client::Health, gvpn_client::Error> {
    let client = reqwest::Client::new();
    let socket_addr = session.bound_host;
    let timeout = options.timeouts.http;
    tracing::debug!(?socket_addr, ?timeout, "requesting health status from exit");
    gvpn_client::health(&client, socket_addr, timeout).await
}

async fn close_health_session(hopr: &Hopr, session: &SessionClientMetadata) {
    tracing::debug!(bound_host = ?session.bound_host, "closing TCP session from health check");
    let _ = hopr
        .close_session(session.bound_host, session.protocol)
        .await
        .map_err(|err| {
            tracing::warn!(error = ?err, "failed to close health session");
            err
        });
}

// ---------------------------------------------------------------------------
// ExitHealth scheduling
// ---------------------------------------------------------------------------

impl ExitHealth {
    pub fn next_interval(&self, ping_interval: Duration) -> Option<Duration> {
        match self {
            ExitHealth::Init | ExitHealth::Checking { .. } => None,
            ExitHealth::Unhealthy { previous_failures, .. } => {
                let interval =
                    (FAILURE_INTERVAL + FAILURE_INTERVAL * (*previous_failures)).min(MAX_INTERVAL_BETWEEN_FAILURES);
                Some(interval)
            }
            ExitHealth::Healthy { .. } => Some(ping_interval),
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions for Core
// ---------------------------------------------------------------------------

pub fn any_needs_peers<'a>(healths: impl Iterator<Item = &'a RouteHealth>) -> bool {
    healths.into_iter().any(|rh| rh.needs_peer())
}

pub fn count_distinct_channels<'a>(healths: impl Iterator<Item = &'a RouteHealth>) -> usize {
    let mut addresses = HashSet::new();
    let mut has_any_channel = false;

    for rh in healths {
        match &rh.state {
            RouteHealthState::NeedsFunding {
                need: ChannelNeed::Channel(addr),
                ..
            }
            | RouteHealthState::NeedsPeering {
                need: ChannelNeed::Channel(addr),
                ..
            } => {
                addresses.insert(*addr);
            }
            RouteHealthState::NeedsFunding {
                need: ChannelNeed::AnyChannel,
                ..
            }
            | RouteHealthState::NeedsPeering {
                need: ChannelNeed::AnyChannel,
                ..
            } => {
                has_any_channel = true;
            }
            _ => {}
        }
    }

    let count = addresses.len();
    if count == 0 && has_any_channel { 1 } else { count }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl Display for RouteHealthState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteHealthState::Unrecoverable { reason, .. } => write!(f, "{reason:?}"),
            RouteHealthState::NeedsPeering { need, last_error, .. } => {
                if let Some(err) = last_error {
                    write!(f, "Last error: {err}, ")?;
                }
                match need {
                    ChannelNeed::Channel(addr) => {
                        write!(f, "Needs peered channel to {}", addr.to_checksum())
                    }
                    ChannelNeed::AnyChannel => write!(f, "Needs any peered channel"),
                    ChannelNeed::Peering(addr) => write!(f, "Needs peer {}", addr.to_checksum()),
                }
            }
            RouteHealthState::NeedsFunding { need, last_error, .. } => {
                if let Some(err) = last_error {
                    write!(f, "Last error: {err}, ")?;
                }
                match need {
                    ChannelNeed::Channel(addr) => {
                        write!(f, "Needs funded channel to {}", addr.to_checksum())
                    }
                    ChannelNeed::AnyChannel => write!(f, "Needs any funded channel"),
                    ChannelNeed::Peering(_) => write!(f, "Needs funding"),
                }
            }
            RouteHealthState::Routable { exit, last_error, .. } => {
                if let Some(err) = last_error {
                    write!(f, "Last error: {err}, ")?;
                }
                write!(f, "Routable, exit: {exit}")
            }
            RouteHealthState::ReadyToConnect { exit, last_error, .. } => {
                if let Some(err) = last_error {
                    write!(f, "Last error: {err}, ")?;
                }
                write!(f, "Ready, exit: {exit}")
            }
        }
    }
}

impl Display for ExitHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExitHealth::Init => write!(f, "waiting for check"),
            ExitHealth::Checking { since } => {
                write!(f, "checking since {}", log_output::elapsed(since))
            }
            ExitHealth::Unhealthy {
                checked_at,
                error,
                previous_failures,
            } if *previous_failures > 0 => {
                write!(
                    f,
                    "failed {} times in a row {} ago: {}",
                    previous_failures + 1,
                    log_output::elapsed(checked_at),
                    error,
                )
            }
            ExitHealth::Unhealthy { checked_at, error, .. } => {
                write!(f, "failed {} ago: {}", log_output::elapsed(checked_at), error)
            }
            ExitHealth::Healthy {
                checked_at,
                version,
                ping_rtt,
                health,
                total_time,
            } => {
                write!(
                    f,
                    "{} ago: total {:.2} s, ping {:.2} s",
                    log_output::elapsed(checked_at),
                    total_time.as_secs_f32(),
                    ping_rtt.as_secs_f32(),
                )?;
                if let Some(h) = health {
                    write!(f, ", {h}")?;
                }
                if let Some(v) = version {
                    write!(f, ", {v}")?;
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_route_health(need: ChannelNeed, state_fn: fn(String, ChannelNeed) -> RouteHealthState) -> RouteHealth {
        RouteHealth {
            state: state_fn("test".to_string(), need),
            health_check_cancel: CancellationToken::new(),
            cancel_on_shutdown: CancellationToken::new(),
            check_cycle: 0,
        }
    }

    fn needs_peering(id: String, need: ChannelNeed) -> RouteHealthState {
        RouteHealthState::NeedsPeering {
            id,
            need,
            funded: false,
            exit: ExitHealth::Init,
            last_error: None,
        }
    }

    fn needs_funding(id: String, need: ChannelNeed) -> RouteHealthState {
        RouteHealthState::NeedsFunding {
            id,
            need,
            exit: ExitHealth::Init,
            last_error: None,
        }
    }

    #[test]
    fn test_count_distinct_channels() -> anyhow::Result<()> {
        let addr_1: Address = "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed".parse()?;
        let addr_2: Address = "fb6916095ca1df60bb79ce92ce3ea74c37c5d359".parse()?;

        let rh1 = make_route_health(ChannelNeed::Channel(addr_1), needs_peering);
        let rh2 = make_route_health(ChannelNeed::Channel(addr_2), needs_peering);
        let rh3 = make_route_health(ChannelNeed::Channel(addr_1), needs_peering);
        let rh4 = make_route_health(ChannelNeed::AnyChannel, needs_peering);
        let rh5 = make_route_health(ChannelNeed::Peering(addr_1), needs_peering);

        let all = vec![&rh1, &rh2, &rh3, &rh4, &rh5];
        assert_eq!(count_distinct_channels(all.into_iter()), 2);

        let any_only = vec![&rh4, &rh5];
        assert_eq!(count_distinct_channels(any_only.into_iter()), 1);

        let peering_only = vec![&rh5];
        assert_eq!(count_distinct_channels(peering_only.into_iter()), 0);

        Ok(())
    }

    #[test]
    fn test_count_distinct_channels_funding() -> anyhow::Result<()> {
        let addr_1: Address = "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed".parse()?;

        let rh1 = make_route_health(ChannelNeed::Channel(addr_1), needs_funding);
        let rh2 = make_route_health(ChannelNeed::AnyChannel, needs_funding);

        let all = vec![&rh1, &rh2];
        assert_eq!(count_distinct_channels(all.into_iter()), 1);

        Ok(())
    }
}
