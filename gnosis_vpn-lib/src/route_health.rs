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

/// Returns the first supported API version found in `server_versions`, or `None`
/// if there is no compatible version.
///
/// This is the single place that maps API version strings to gvpn_client modules.
/// Currently only "v1" is supported — all gvpn_client functions use the /api/v1/ prefix.
/// Add new versions here when introducing a new API module.
fn select_api_version(server_versions: &[String]) -> Option<&'static str> {
    const SUPPORTED: &[&str] = &["v1"]; // v1 → gvpn_client
    SUPPORTED.iter().copied().find(|&v| server_versions.iter().any(|sv| sv == v))
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StaticNeed {
    Channel(Address),
    AnyChannel,
    Peering(Address),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UnrecoverableReason {
    NotAllowed,
    InvalidId,
    InvalidPath,
    IncompatibleApiVersion { server_versions: Vec<String> },
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
        versions: gvpn_client::Versions,
        ping_rtt: Duration,
        health: gvpn_client::Health,
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
        reason: UnrecoverableReason,
    },
    /// `funded` remembers whether the channel funding step has already been
    /// completed for this route. On re-peering after a transient peer loss we
    /// can then skip straight to `Routable` instead of triggering redundant
    /// funding ops on every flap.
    NeedsPeering {
        funded: bool,
    },
    NeedsFunding,
    /// Static need met
    Routable {
        exit: ExitHealth,
    },
    /// Exit health confirmed healthy. Safe to connect.
    ReadyToConnect {
        exit: ExitHealth,
    },
    /// Actively connected. TCP health checks suspended, tunnel ping drives ExitHealth.
    Connected {
        exit: ExitHealth,
    },
}

/// Wire-only type used to carry a health-check outcome from the async runner
/// back to `health_check_result`. `version` and `health` are optional here
/// because a given cycle may skip fetching them (see `CheckScope`); the main
/// thread fills in the skipped fields from the previously stored
/// `ExitHealth::Healthy` before constructing the final `ExitHealth`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HealthCheckOutcome {
    Started {
        since: SystemTime,
    },
    Unrecoverable {
        reason: UnrecoverableReason,
    },
    Failed {
        checked_at: SystemTime,
        error: String,
    },
    Completed {
        checked_at: SystemTime,
        versions: Option<gvpn_client::Versions>,
        ping_rtt: Duration,
        health: Option<gvpn_client::Health>,
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
    id: String,
    static_need: StaticNeed,
    state: RouteHealthState,
    health_check_cancel: CancellationToken,
    cancel_on_shutdown: CancellationToken,
    check_cycle: u32,
    last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl RouteHealth {
    pub fn new(dest: &Destination, allow_insecure: bool, cancel_on_shutdown: CancellationToken) -> Self {
        let static_need = derive_static_need(&dest.routing, dest.address);
        let state = derive_initial_state(&dest.routing, allow_insecure);
        Self {
            id: dest.id.clone(),
            static_need,
            state,
            health_check_cancel: CancellationToken::new(),
            cancel_on_shutdown,
            check_cycle: 0,
            last_error: None,
        }
    }
}

/// Derive the static need from routing alone. For invalid routing variants
/// (empty path / offchain-first), we fall back to `AnyChannel` — the resulting
/// state will be `Unrecoverable` so the field is never observed.
fn derive_static_need(routing: &RoutingOptions, dest_address: Address) -> StaticNeed {
    match routing.clone() {
        RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 => StaticNeed::Peering(dest_address),
        RoutingOptions::Hops(_) => StaticNeed::AnyChannel,
        RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
            Some(NodeId::Chain(address)) => StaticNeed::Channel(address),
            _ => StaticNeed::AnyChannel,
        },
    }
}

fn derive_initial_state(routing: &RoutingOptions, allow_insecure: bool) -> RouteHealthState {
    match routing.clone() {
        RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 && !allow_insecure => {
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::NotAllowed,
            }
        }
        RoutingOptions::Hops(_) => RouteHealthState::NeedsPeering { funded: false },
        RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
            Some(NodeId::Chain(_)) => RouteHealthState::NeedsPeering { funded: false },
            Some(NodeId::Offchain(_)) => RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::InvalidId,
            },
            None => RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::InvalidPath,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

impl RouteHealth {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn state(&self) -> &RouteHealthState {
        &self.state
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn needs_peer(&self) -> bool {
        matches!(self.state, RouteHealthState::NeedsPeering { .. })
    }

    pub fn needs_channel_funding(&self) -> Option<Address> {
        match (&self.state, &self.static_need) {
            (RouteHealthState::NeedsFunding, StaticNeed::Channel(addr)) => Some(*addr),
            _ => None,
        }
    }

    pub fn needs_any_channel_funding(&self) -> bool {
        matches!(self.state, RouteHealthState::NeedsFunding) && matches!(self.static_need, StaticNeed::AnyChannel)
    }

    pub fn is_routable(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::Routable { .. }
                | RouteHealthState::ReadyToConnect { .. }
                | RouteHealthState::Connected { .. }
        )
    }

    pub fn is_ready_to_connect(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::ReadyToConnect { .. } | RouteHealthState::Connected { .. }
        )
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
        let is_peered = match &self.static_need {
            StaticNeed::Channel(addr) | StaticNeed::Peering(addr) => addresses.contains(addr),
            StaticNeed::AnyChannel => !addresses.is_empty(),
        };

        match &self.state {
            RouteHealthState::NeedsPeering { funded } => {
                if !is_peered {
                    return PeerTransition::NoChange;
                }
                // Peering needs do not require funding. Channel/AnyChannel needs
                // can also skip funding when we already funded earlier in this
                // route's lifetime (transient peer flap).
                let skip_funding = matches!(self.static_need, StaticNeed::Peering(_)) || *funded;
                if skip_funding {
                    self.state = RouteHealthState::Routable { exit: ExitHealth::Init };
                    self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
                    PeerTransition::BecameRoutable
                } else {
                    self.state = RouteHealthState::NeedsFunding;
                    PeerTransition::NowNeedsFunding
                }
            }
            RouteHealthState::NeedsFunding => {
                if is_peered {
                    PeerTransition::NoChange
                } else {
                    // Funding never completed, so leave `funded: false`.
                    self.state = RouteHealthState::NeedsPeering { funded: false };
                    PeerTransition::LostPeer
                }
            }
            RouteHealthState::Routable { .. }
            | RouteHealthState::ReadyToConnect { .. }
            | RouteHealthState::Connected { .. } => {
                if is_peered {
                    PeerTransition::NoChange
                } else {
                    self.cancel_health_check();
                    // We previously made it past funding (or never needed it),
                    // so remember that to avoid re-funding on re-peer.
                    self.state = RouteHealthState::NeedsPeering { funded: true };
                    PeerTransition::LostPeer
                }
            }
            RouteHealthState::Unrecoverable { .. } => PeerTransition::NoChange,
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
        if !matches!(self.state, RouteHealthState::NeedsFunding) {
            return;
        }
        let satisfies_need = match &self.static_need {
            StaticNeed::Channel(addr) => *addr == address,
            StaticNeed::AnyChannel => true,
            StaticNeed::Peering(_) => false,
        };
        if satisfies_need {
            self.state = RouteHealthState::Routable { exit: ExitHealth::Init };
            self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
        }
    }

    pub fn health_check_result(
        &mut self,
        outcome: HealthCheckOutcome,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        // Only process outcomes when actively running health checks
        if !matches!(
            self.state,
            RouteHealthState::Routable { .. } | RouteHealthState::ReadyToConnect { .. }
        ) {
            return;
        }

        // Unrecoverable outcomes permanently terminate health checking for this route.
        if let HealthCheckOutcome::Unrecoverable { reason } = outcome {
            self.cancel_health_check();
            self.state = RouteHealthState::Unrecoverable { reason };
            return;
        }

        // Lift outcome into an ExitHealth, filling in skipped fields from the
        // previously stored Healthy state.
        let prior = self.exit_ref().cloned();
        let merged = match outcome {
            HealthCheckOutcome::Started { since } => ExitHealth::Checking { since },
            HealthCheckOutcome::Unrecoverable { .. } => unreachable!("handled above"),
            HealthCheckOutcome::Failed { checked_at, error } => {
                let previous_failures = match &prior {
                    Some(ExitHealth::Unhealthy { previous_failures, .. }) => previous_failures + 1,
                    _ => 0,
                };
                ExitHealth::Unhealthy {
                    checked_at,
                    error,
                    previous_failures,
                }
            }
            HealthCheckOutcome::Completed {
                checked_at,
                versions,
                ping_rtt,
                health,
            } => {
                // Carry forward version/health from the previous Healthy
                // when this cycle skipped fetching them.
                let (prior_version, prior_health) = match &prior {
                    Some(ExitHealth::Healthy {
                        versions: v, health: h, ..
                    }) => (Some(v.clone()), Some(h.clone())),
                    _ => (None, None),
                };
                match (versions.or(prior_version), health.or(prior_health)) {
                    (Some(versions), Some(health)) => ExitHealth::Healthy {
                        checked_at,
                        versions,
                        ping_rtt,
                        health,
                    },
                    // Missing data we cannot recover from prior state — treat
                    // as a check failure so the state machine retries and
                    // captures fresh values.
                    _ => ExitHealth::Unhealthy {
                        checked_at,
                        error: "health check skipped version/health without a prior Healthy to carry forward"
                            .to_string(),
                        previous_failures: match &prior {
                            Some(ExitHealth::Unhealthy { previous_failures, .. }) => previous_failures + 1,
                            _ => 0,
                        },
                    },
                }
            }
        };

        // Reflect the latest health-check error in the top-level last_error
        match &merged {
            ExitHealth::Healthy { .. } => self.last_error = None,
            ExitHealth::Unhealthy { error, .. } => self.last_error = Some(error.clone()),
            ExitHealth::Init | ExitHealth::Checking { .. } => {}
        }

        let ping_interval = options.health_check_intervals.ping;
        let next_interval = merged.next_interval(ping_interval);
        let is_healthy = matches!(merged, ExitHealth::Healthy { .. });

        // Only advance the check cycle on success so that a failed check
        // retries with the same scope (version/health) instead of
        // downgrading to a ping-only check on the next attempt.
        if is_healthy {
            self.check_cycle = self.check_cycle.wrapping_add(1);
        }

        self.set_exit(merged);

        // Transition Routable ↔ ReadyToConnect based on exit health
        match &self.state {
            RouteHealthState::Routable { exit } if is_healthy => {
                self.state = RouteHealthState::ReadyToConnect { exit: exit.clone() };
            }
            RouteHealthState::ReadyToConnect { exit, .. } if !is_healthy => {
                self.state = RouteHealthState::Routable { exit: exit.clone() };
            }
            _ => {}
        }

        if let Some(delay) = next_interval {
            self.spawn_health_check(delay, hopr, dest, options, sender);
        }
    }

    /// Transition ReadyToConnect → Connected, cancel TCP health checks, clear errors.
    pub fn connected(&mut self) {
        if matches!(self.state, RouteHealthState::ReadyToConnect { .. }) {
            self.cancel_health_check();
            self.last_error = None;
            self.state = RouteHealthState::Connected { exit: ExitHealth::Init };
        }
    }

    /// Transition Connected → Routable, restart TCP health checks.
    /// Goes via Routable (not ReadyToConnect) because the API version is
    /// only known after a fresh successful health check.
    pub fn disconnected(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if matches!(self.state, RouteHealthState::Connected { .. }) {
            self.state = RouteHealthState::Routable { exit: ExitHealth::Init };
            self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
        }
    }

    /// Update exit health from a tunnel ping result. Returns the consecutive
    /// failure count after applying this result. On success, exit health is
    /// left unchanged because a tunnel ping by itself does not produce the
    /// `version` / `health` data required to construct an `ExitHealth::Healthy`.
    pub fn tunnel_ping_result(&mut self, rtt: Result<Duration, String>) -> u32 {
        if !matches!(self.state, RouteHealthState::Connected { .. }) {
            return 0;
        }
        match rtt {
            Ok(_) => {
                self.last_error = None;
                0
            }
            Err(err) => {
                let prev = match self.exit_ref() {
                    Some(ExitHealth::Unhealthy { previous_failures, .. }) => *previous_failures + 1,
                    _ => 1,
                };
                self.set_exit(ExitHealth::Unhealthy {
                    checked_at: SystemTime::now(),
                    error: err.clone(),
                    previous_failures: prev,
                });
                self.last_error = Some(err);
                prev
            }
        }
    }

    pub fn with_error(&mut self, err: String) {
        if matches!(self.state, RouteHealthState::Unrecoverable { .. }) {
            return;
        }
        self.last_error = Some(err);
    }

    fn exit_ref(&self) -> Option<&ExitHealth> {
        match &self.state {
            RouteHealthState::Routable { exit }
            | RouteHealthState::ReadyToConnect { exit, .. }
            | RouteHealthState::Connected { exit } => Some(exit),
            _ => None,
        }
    }

    fn set_exit(&mut self, new_exit: ExitHealth) {
        match &mut self.state {
            RouteHealthState::Routable { exit }
            | RouteHealthState::ReadyToConnect { exit, .. }
            | RouteHealthState::Connected { exit } => {
                *exit = new_exit;
            }
            _ => {}
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
                    tracing::info!(%dest, ?scope, "starting health check");
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
            outcome: HealthCheckOutcome::Started {
                since: SystemTime::now(),
            },
        })
        .await;

    let checked_at = SystemTime::now();

    let res_session = open_health_session(hopr, destination, options).await;
    let session = match res_session {
        Ok(session) => session,
        Err(err) => {
            let _ = sender
                .send(Results::HealthCheck {
                    id,
                    outcome: HealthCheckOutcome::Failed {
                        checked_at,
                        error: format!("Session creation error: {err}"),
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
    let mut versions = None;
    if scope.version {
        match gvpn_client::versions(&client, socket_addr, timeout).await {
            Ok(v) => {
                if select_api_version(&v.versions).is_none() {
                    close_health_session(hopr, &session).await;
                    tracing::warn!(%destination, server_versions = %v, "exit server offers no compatible API version");
                    let _ = sender
                        .send(Results::HealthCheck {
                            id,
                            outcome: HealthCheckOutcome::Unrecoverable {
                                reason: UnrecoverableReason::IncompatibleApiVersion {
                                    server_versions: v.versions.clone(),
                                },
                            },
                        })
                        .await;
                    return;
                }
                tracing::debug!(%destination, versions = %v, "exit server version check passed");
                versions = Some(v);
            }
            Err(err) => {
                close_health_session(hopr, &session).await;
                let _ = sender
                    .send(Results::HealthCheck {
                        id,
                        outcome: HealthCheckOutcome::Failed {
                            checked_at,
                            error: format!("Version check error: {err}"),
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
                outcome: HealthCheckOutcome::Failed {
                    checked_at,
                    error: format!("Ping error: {err}"),
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
                        outcome: HealthCheckOutcome::Failed {
                            checked_at,
                            error: format!("Health request error: {err}"),
                        },
                    })
                    .await;
                return;
            }
        }
    }

    close_health_session(hopr, &session).await;

    let _ = sender
        .send(Results::HealthCheck {
            id,
            outcome: HealthCheckOutcome::Completed {
                checked_at,
                versions,
                ping_rtt,
                health,
            },
        })
        .await;
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
    fn next_interval(&self, ping_interval: Duration) -> Option<Duration> {
        match self {
            ExitHealth::Init | ExitHealth::Checking { .. } => None,
            ExitHealth::Unhealthy { previous_failures, .. } => {
                let interval = (FAILURE_INTERVAL + FAILURE_INTERVAL * (*previous_failures))
                    .min(MAX_INTERVAL_BETWEEN_FAILURES);
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
        let still_needs_channel = matches!(
            rh.state,
            RouteHealthState::NeedsPeering { .. } | RouteHealthState::NeedsFunding
        );
        if !still_needs_channel {
            continue;
        }
        match &rh.static_need {
            StaticNeed::Channel(addr) => {
                addresses.insert(*addr);
            }
            StaticNeed::AnyChannel => {
                has_any_channel = true;
            }
            StaticNeed::Peering(_) => {}
        }
    }

    let count = addresses.len();
    if count == 0 && has_any_channel { 1 } else { count }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl Display for UnrecoverableReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UnrecoverableReason::NotAllowed => write!(f, "direct peering not allowed (insecure peering disabled)"),
            UnrecoverableReason::InvalidId => write!(f, "path contains offchain node ID (unsupported)"),
            UnrecoverableReason::InvalidPath => write!(f, "path is empty"),
            UnrecoverableReason::IncompatibleApiVersion { server_versions } => {
                write!(f, "exit server offers no compatible API version (server offers: {})", server_versions.join(", "))
            }
        }
    }
}

impl Display for RouteHealthState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteHealthState::Unrecoverable { reason } => write!(f, "Unrecoverable: {reason}"),
            RouteHealthState::NeedsPeering { funded: false } => write!(f, "Needs peering"),
            RouteHealthState::NeedsPeering { funded: true } => write!(f, "Needs peering (channel funded)"),
            RouteHealthState::NeedsFunding => write!(f, "Needs funding"),
            RouteHealthState::Routable { exit } => write!(f, "Routable, exit: {exit}"),
            RouteHealthState::ReadyToConnect { exit } => match exit {
                ExitHealth::Healthy { versions, .. } => {
                    let selected = select_api_version(&versions.versions).unwrap_or(&versions.latest);
                    let available = versions.versions.join(", ");
                    write!(f, "Ready (API {selected} of [{available}]), exit: {exit}")
                }
                _ => write!(f, "Ready, exit: {exit}"),
            },
            RouteHealthState::Connected { exit } => write!(f, "Connected, tunnel: {exit}"),
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
                versions,
                ping_rtt,
                health,
            } => {
                write!(
                    f,
                    "{} ago: ping {:.2} s, {}, {}",
                    log_output::elapsed(checked_at),
                    ping_rtt.as_secs_f32(),
                    health,
                    versions,
                )
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

    fn make_route_health(static_need: StaticNeed, state: RouteHealthState) -> RouteHealth {
        RouteHealth {
            id: "test".to_string(),
            static_need,
            state,
            health_check_cancel: CancellationToken::new(),
            cancel_on_shutdown: CancellationToken::new(),
            check_cycle: 0,
            last_error: None,
        }
    }

    #[test]
    fn test_count_distinct_channels() -> anyhow::Result<()> {
        let addr_1: Address = "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed".parse()?;
        let addr_2: Address = "fb6916095ca1df60bb79ce92ce3ea74c37c5d359".parse()?;

        let rh1 = make_route_health(
            StaticNeed::Channel(addr_1),
            RouteHealthState::NeedsPeering { funded: false },
        );
        let rh2 = make_route_health(
            StaticNeed::Channel(addr_2),
            RouteHealthState::NeedsPeering { funded: false },
        );
        let rh3 = make_route_health(
            StaticNeed::Channel(addr_1),
            RouteHealthState::NeedsPeering { funded: false },
        );
        let rh4 = make_route_health(StaticNeed::AnyChannel, RouteHealthState::NeedsPeering { funded: false });
        let rh5 = make_route_health(
            StaticNeed::Peering(addr_1),
            RouteHealthState::NeedsPeering { funded: false },
        );

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

        let rh1 = make_route_health(StaticNeed::Channel(addr_1), RouteHealthState::NeedsFunding);
        let rh2 = make_route_health(StaticNeed::AnyChannel, RouteHealthState::NeedsFunding);

        let all = vec![&rh1, &rh2];
        assert_eq!(count_distinct_channels(all.into_iter()), 1);

        Ok(())
    }
}
