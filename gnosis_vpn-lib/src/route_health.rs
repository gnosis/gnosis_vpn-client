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
use crate::core::runner::{Results, to_surb_balancer_config};
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
    SUPPORTED
        .iter()
        .copied()
        .find(|&v| server_versions.iter().any(|sv| sv == v))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitHealth {
    pub checked_at: SystemTime,
    pub versions: gvpn_client::Versions,
    pub ping_rtt: Duration,
    pub health: gvpn_client::Health,
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
    /// Static need met. Health checking in progress.
    Routable,
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
    checking_since: Option<SystemTime>,
    consecutive_failures: u32,
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
            checking_since: None,
            consecutive_failures: 0,
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

    pub fn checking_since(&self) -> Option<SystemTime> {
        self.checking_since
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
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
            RouteHealthState::Routable | RouteHealthState::ReadyToConnect { .. } | RouteHealthState::Connected { .. }
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
                    self.state = RouteHealthState::Routable;
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
            RouteHealthState::Routable
            | RouteHealthState::ReadyToConnect { .. }
            | RouteHealthState::Connected { .. } => {
                if is_peered {
                    PeerTransition::NoChange
                } else {
                    self.cancel_health_check();
                    self.checking_since = None;
                    self.consecutive_failures = 0;
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
            self.state = RouteHealthState::Routable;
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
        if !matches!(
            self.state,
            RouteHealthState::Routable | RouteHealthState::ReadyToConnect { .. }
        ) {
            return;
        }

        if let HealthCheckOutcome::Started { since } = outcome {
            self.checking_since = Some(since);
            return;
        }

        self.checking_since = None;

        if let HealthCheckOutcome::Unrecoverable { reason } = outcome {
            self.cancel_health_check();
            self.state = RouteHealthState::Unrecoverable { reason };
            return;
        }

        match outcome {
            HealthCheckOutcome::Started { .. } | HealthCheckOutcome::Unrecoverable { .. } => {
                unreachable!("handled above")
            }
            HealthCheckOutcome::Failed { error, .. } => {
                self.consecutive_failures += 1;
                self.last_error = Some(error);

                if matches!(self.state, RouteHealthState::ReadyToConnect { .. }) {
                    self.state = RouteHealthState::Routable;
                }

                let delay = self.failure_backoff();
                self.spawn_health_check(delay, hopr, dest, options, sender);
            }
            HealthCheckOutcome::Completed {
                checked_at,
                versions,
                ping_rtt,
                health,
            } => {
                // Carry forward version/health from the previous exit
                // when this cycle skipped fetching them.
                let prior = self.exit_ref().cloned();
                let (prior_versions, prior_health) = match &prior {
                    Some(exit) => (Some(exit.versions.clone()), Some(exit.health.clone())),
                    None => (None, None),
                };
                match (versions.or(prior_versions), health.or(prior_health)) {
                    (Some(versions), Some(health)) => {
                        let exit = ExitHealth {
                            checked_at,
                            versions,
                            ping_rtt,
                            health,
                        };
                        self.consecutive_failures = 0;
                        self.last_error = None;
                        self.check_cycle = self.check_cycle.wrapping_add(1);
                        self.state = RouteHealthState::ReadyToConnect { exit };

                        let delay = options.health_check_intervals.ping;
                        self.spawn_health_check(delay, hopr, dest, options, sender);
                    }
                    // check_cycle only advances on success, so a ping-only cycle
                    // can only run after a prior success. Reaching here means
                    // that invariant was broken.
                    _ => unreachable!(
                        "ping-only cycle ran without a prior Healthy; \
                         check_cycle must only advance on success"
                    ),
                }
            }
        }
    }

    /// Transition ReadyToConnect → Connected, cancel TCP health checks, clear errors.
    pub fn connected(&mut self) {
        if let RouteHealthState::ReadyToConnect { exit } = &self.state {
            let exit = exit.clone();
            self.cancel_health_check();
            self.checking_since = None;
            self.consecutive_failures = 0;
            self.last_error = None;
            self.state = RouteHealthState::Connected { exit };
        }
    }

    /// Transition Connected → ReadyToConnect (if no recent failures) or Routable,
    /// and restart TCP health checks.
    pub fn disconnected(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if let RouteHealthState::Connected { exit } = &self.state {
            let exit = exit.clone();
            if self.consecutive_failures == 0 {
                self.state = RouteHealthState::ReadyToConnect { exit };
            } else {
                self.state = RouteHealthState::Routable;
            }
            self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
        }
    }

    /// Update exit health from a tunnel ping result. Returns the consecutive
    /// failure count after applying this result. On success the `ping_rtt` is
    /// refreshed with the new measurement. On failure the exit data is
    /// preserved and `consecutive_failures` is incremented.
    pub fn tunnel_ping_result(&mut self, rtt: Result<Duration, String>) -> u32 {
        if let RouteHealthState::Connected { exit } = &mut self.state {
            match rtt {
                Ok(rtt) => {
                    self.last_error = None;
                    self.consecutive_failures = 0;
                    exit.ping_rtt = rtt;
                    0
                }
                Err(err) => {
                    self.consecutive_failures += 1;
                    self.last_error = Some(err);
                    self.consecutive_failures
                }
            }
        } else {
            0
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
            RouteHealthState::ReadyToConnect { exit } | RouteHealthState::Connected { exit } => Some(exit),
            _ => None,
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

    tracing::warn!(?session, "FOO_SESSION");
    tracing::warn!(?scope, "FOO_SCOPE");

    let socket_addr = session.bound_host;
    let timeout = options.timeouts.http;
    let client = reqwest::Client::new();

    // Step 1: Version check (when due)
    let mut versions = None;
    if scope.version {
        let res_versions = gvpn_client::versions(&client, socket_addr, timeout).await;
        tracing::warn!(?res_versions, "FOO_CALL1 VERSIONS");
        match res_versions {
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

    // Step 2: Exit health (when due)
    let mut health = None;
    if scope.health {
        let res_health = request_health(options, &session).await;
        tracing::warn!(?res_health, "FOO_CALL2 HEALTH");
        match res_health {
            Ok(h) => {
                tracing::debug!(%destination, health = %h, "received exit health status");
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

    // Step 3: Ping (always runs)
    let measure_rtt = Instant::now();
    let res_ping = gvpn_client::ping(&client, socket_addr, timeout).await;
    tracing::warn!(?res_ping, "FOO_CALL3 PING");
    let initial_ping_rtt = measure_rtt.elapsed();
    tracing::warn!(?initial_ping_rtt, "FOO_INITIAL_PING_RTT");

    // Step 4: Only for debugging showcase - remove
    let measure_rtt_2 = Instant::now();
    let res_ping_2 = gvpn_client::ping(&client, socket_addr, timeout).await;
    tracing::warn!(?res_ping_2, "FOO_CALL4 PING2");
    let ping_rtt = measure_rtt_2.elapsed();
    tracing::warn!(?ping_rtt, "FOO_SECOND_PING_RTT");

    close_health_session(hopr, &session).await;

    match res_ping {
        Ok(_) => {
            tracing::debug!(%destination, ping_rtt = ?ping_rtt, "exit ping successful");
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
        Err(err) => {
            tracing::info!(%destination, error = %err, "exit ping failed");
            let _ = sender
                .send(Results::HealthCheck {
                    id,
                    outcome: HealthCheckOutcome::Failed {
                        checked_at,
                        error: format!("Ping error: {err}"),
                    },
                })
                .await;
        }
    }
}

async fn open_health_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
) -> Result<SessionClientMetadata, HoprError> {
    let surb_config = to_surb_balancer_config(options.buffer_sizes.bridge, options.max_surb_upstream.bridge)
        .map_err(|e| HoprError::Construction(e.to_string()))?;

    let p_surb_config = Some(surb_config);
    let p_session_pool = None;
    let p_max_client_sessions = None;
    tracing::warn!(?p_surb_config, "FOO_SESSION_CONFIG");
    tracing::warn!(?p_session_pool, "FOO_SESSION_POOL");
    tracing::warn!(?p_max_client_sessions, "FOO_MAX_CLIENT_SESSIONS");

    let cfg = SessionClientConfig {
        capabilities: options.sessions.health.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: p_surb_config,
        ..Default::default()
    };
    tracing::debug!(%destination, "opening TCP session for health check");
    hopr.open_session(
        destination.address,
        options.sessions.health.target.clone(),
        p_session_pool,
        p_max_client_sessions,
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
// RouteHealth scheduling
// ---------------------------------------------------------------------------

impl RouteHealth {
    fn failure_backoff(&self) -> Duration {
        (FAILURE_INTERVAL + FAILURE_INTERVAL * self.consecutive_failures).min(MAX_INTERVAL_BETWEEN_FAILURES)
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
                write!(
                    f,
                    "exit server offers no compatible API version (server offers: {})",
                    server_versions.join(", ")
                )
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
            RouteHealthState::NeedsFunding => write!(f, "Needs channel funding"),
            RouteHealthState::Routable => write!(f, "Routable - checking exit health"),
            RouteHealthState::ReadyToConnect { exit } => {
                let selected = select_api_version(&exit.versions.versions).unwrap_or(&exit.versions.latest);
                write!(f, "Ready to connect via API {selected}, exit health: {exit}")
            }
            RouteHealthState::Connected { exit } => write!(f, "Connected, tunnel: {exit}"),
        }
    }
}

impl Display for ExitHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} ago: ping {:.2} s, {}, {}",
            log_output::elapsed(&self.checked_at),
            self.ping_rtt.as_secs_f32(),
            self.health,
            self.versions,
        )
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
            checking_since: None,
            consecutive_failures: 0,
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
