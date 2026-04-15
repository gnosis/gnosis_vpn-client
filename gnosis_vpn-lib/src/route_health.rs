//! Per-destination route health tracking.
//!
//! Each [`RouteHealth`] models the progression of a single destination route
//! from "just configured" to "usable for a tunnel", and it owns the background
//! health-check task that keeps that assessment current.
//!
//! The progression is split into two concerns:
//!
//! * **Network reachability** — do we have the peering/channel relationship
//!   that the routing option requires? This is driven from outside by Core
//!   feeding in the current peer set ([`RouteHealth::peers`]) and channel
//!   funding results ([`RouteHealth::channel_funded`]).
//! * **Exit-node health** — once reachable, can we actually reach the exit
//!   server behind the destination, and is it reporting healthy? This is
//!   driven internally by a background task that opens a short-lived TCP
//!   session to the exit and performs version, health, and ping checks.
//!
//! [`RouteHealthState`] captures the combined state. State changes flow
//! outward through [`PeerTransition`] return values and through
//! [`HealthCheckOutcome`] messages posted back on the runner channel.
//!
//! Core owns one `RouteHealth` per configured destination and uses the
//! aggregate view (via [`any_needs_peers`] / [`count_distinct_channels`]) to
//! decide when to poll peers or fund channels.
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
    SUPPORTED
        .iter()
        .copied()
        .find(|&v| server_versions.iter().any(|sv| sv == v))
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The on-chain precondition a route needs before it can be considered
/// reachable. Derived once from routing configuration and then constant for
/// the lifetime of the `RouteHealth`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StaticNeed {
    /// A funded channel to this specific address (first intermediate hop).
    Channel(Address),
    /// Any funded outgoing channel is sufficient (hop count without a fixed path).
    AnyChannel,
    /// Direct peering with the destination — no channel needed (0-hop route).
    Peering(Address),
}

/// Terminal failure modes that cannot be recovered from without a config
/// change or an exit-server upgrade. Once a route enters
/// [`RouteHealthState::Unrecoverable`] it stays there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UnrecoverableReason {
    /// Direct (0-hop) peering is configured but insecure peering is disabled.
    NotAllowed,
    /// The configured path contains an offchain node ID, which is not supported.
    InvalidId,
    /// The configured intermediate path is empty.
    InvalidPath,
    /// The exit server only offers API versions we do not support.
    IncompatibleApiVersion { server_versions: Vec<String> },
}

/// A successfully captured snapshot of exit-node health.
///
/// Not every check cycle fetches every field (see [`CheckScope`]); when a
/// field is skipped it is carried forward from the previous successful
/// snapshot so the state always exposes a full picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitHealth {
    pub checked_at: SystemTime,
    pub versions: gvpn_client::Versions,
    pub ping_rtt: Duration,
    pub health: gvpn_client::Health,
}

/// Combined route state: network reachability plus exit-node health.
///
/// Also the wire-format shown to the CLI via the command API, so variant
/// names and payloads are part of the user-visible surface.
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
    /// Connecting or connected. Exit health and ping checks continue at reduced
    /// frequency (version skipped).
    Connecting {
        exit: ExitHealth,
        tunnel_ping_rtt: Option<Duration>,
    },
}

/// Message a health-check runner task sends back to the main loop, consumed
/// by [`RouteHealth::health_check_result`].
///
/// `versions` and `health` are optional because a given cycle may skip
/// fetching them (see [`CheckScope`]); the main thread fills in the skipped
/// fields from the previously stored [`ExitHealth`] before constructing the
/// final snapshot.
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
        ping_rtt: Option<Duration>,
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

/// Per-destination route health tracker.
///
/// Owns the health-check lifecycle: state transitions, the background
/// task's cancellation token, and failure bookkeeping used for backoff.
/// Constructed once per destination and lives as long as the destination
/// is configured.
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
    /// Build an initial tracker for `dest`. `cancel_on_shutdown` is inherited
    /// by every background task this tracker spawns so that they all stop
    /// when the core shuts down. `allow_insecure` gates whether a 0-hop route
    /// is accepted or immediately marked unrecoverable.
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

/// Pick the starting state purely from routing config. Invalid or
/// disallowed routing variants short-circuit straight to `Unrecoverable`;
/// everything else starts at `NeedsPeering` and waits for Core to feed in
/// the peer set.
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
            RouteHealthState::Routable | RouteHealthState::ReadyToConnect { .. } | RouteHealthState::Connecting { .. }
        )
    }

    pub fn is_ready_to_connect(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::ReadyToConnect { .. } | RouteHealthState::Connecting { .. }
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
    /// Apply a fresh snapshot of connected peer addresses.
    ///
    /// Advances or regresses the state depending on whether the route's
    /// [`StaticNeed`] is currently satisfied. When a previously funded route
    /// loses its peer we transition back to `NeedsPeering { funded: true }`
    /// so that re-peering can skip funding. When the route first becomes
    /// routable we spawn the initial health check.
    ///
    /// The returned [`PeerTransition`] tells Core which external side effect
    /// (if any) needs to run — Core handles funding, the tracker handles
    /// health-check spawn/cancel internally.
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
            | RouteHealthState::Connecting { .. } => {
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

    /// Notify that a channel funding operation succeeded for `address`.
    ///
    /// If this route was waiting on that funding (or on any funding, for
    /// `AnyChannel` needs) it becomes routable and the first health check
    /// is scheduled immediately. Calls that do not apply to this route are
    /// ignored.
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

    /// Consume an outcome from a background health-check cycle and schedule
    /// the next one.
    ///
    /// Handles three concerns together:
    ///
    /// * Lifecycle: `Started` records the "checking since" timestamp;
    ///   terminal outcomes clear it.
    /// * State transitions: a successful full cycle promotes `Routable` →
    ///   `ReadyToConnect`; a failure demotes `ReadyToConnect` back to
    ///   `Routable` (during `Connecting` the state is kept and only the
    ///   failure counter moves). `Unrecoverable` is honored only outside
    ///   `Connecting`.
    /// * Scheduling: success schedules the next cycle at the configured
    ///   ping interval; failure schedules with [`Self::failure_backoff`].
    ///
    /// Outcomes that arrive when the state is no longer `Routable` /
    /// `ReadyToConnect` / `Connecting` (e.g. because peering was lost)
    /// are dropped.
    pub fn health_check_result(
        &mut self,
        outcome: HealthCheckOutcome,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        let is_connecting = matches!(self.state, RouteHealthState::Connecting { .. });
        if !is_connecting
            && !matches!(
                self.state,
                RouteHealthState::Routable | RouteHealthState::ReadyToConnect { .. }
            )
        {
            return;
        }

        if let HealthCheckOutcome::Started { since } = outcome {
            self.checking_since = Some(since);
            return;
        }

        self.checking_since = None;

        if !is_connecting && let HealthCheckOutcome::Unrecoverable { reason } = outcome {
            self.cancel_health_check();
            self.state = RouteHealthState::Unrecoverable { reason };
            return;
        }

        match outcome {
            HealthCheckOutcome::Started { .. } | HealthCheckOutcome::Unrecoverable { .. } => {
                // Started handled above. Unrecoverable ignored while connecting
                // (version check is skipped so it shouldn't occur).
                let delay = self.failure_backoff();
                self.spawn_health_check(delay, hopr, dest, options, sender);
            }
            HealthCheckOutcome::Failed { error, .. } if is_connecting => {
                self.consecutive_failures += 1;
                self.last_error = Some(error);
                let delay = self.failure_backoff();
                self.spawn_health_check(delay, hopr, dest, options, sender);
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
                ping_rtt,
                health,
                ..
            } if is_connecting => {
                if let RouteHealthState::Connecting { exit, .. } = &mut self.state {
                    if let Some(h) = health {
                        exit.health = h;
                    }
                    if let Some(rtt) = ping_rtt {
                        exit.ping_rtt = rtt;
                    }
                    exit.checked_at = checked_at;
                }
                self.consecutive_failures = 0;
                self.last_error = None;
                self.check_cycle = self.check_cycle.wrapping_add(1);
                let intervals = &options.health_check_intervals;
                let delay = intervals.ping * intervals.health_every_n_pings;
                self.spawn_health_check(delay, hopr, dest, options, sender);
            }
            HealthCheckOutcome::Completed {
                checked_at,
                versions,
                ping_rtt,
                health,
            } => {
                // Carry forward version/health/ping_rtt from the previous exit
                // when this cycle skipped fetching them.
                let prior = self.exit_ref().cloned();
                let (prior_versions, prior_health, prior_ping_rtt) = match &prior {
                    Some(exit) => (
                        Some(exit.versions.clone()),
                        Some(exit.health.clone()),
                        Some(exit.ping_rtt),
                    ),
                    None => (None, None, None),
                };
                match (
                    versions.or(prior_versions),
                    health.or(prior_health),
                    ping_rtt.or(prior_ping_rtt),
                ) {
                    (Some(versions), Some(health), Some(ping_rtt)) => {
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

    /// Transition `ReadyToConnect` → `Connecting` when Core starts bringing
    /// up the tunnel.
    ///
    /// While connecting we stop verifying the API version and reduce the
    /// check cadence: only an exit-health query runs in each cycle, on top
    /// of the tunnel-level ping Core performs. Other states are left
    /// unchanged so this is safe to call speculatively.
    pub fn connecting(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if let RouteHealthState::ReadyToConnect { exit } = &self.state {
            let exit = exit.clone();
            self.cancel_health_check();
            self.checking_since = None;
            self.consecutive_failures = 0;
            self.last_error = None;
            self.state = RouteHealthState::Connecting {
                exit,
                tunnel_ping_rtt: None,
            };
            let delay = options.health_check_intervals.ping;
            self.spawn_health_check(delay, hopr, dest, options, sender);
        }
    }

    /// Leave `Connecting` and resume normal health checking.
    ///
    /// The resulting state depends on whether the route is still considered
    /// healthy: no recent failures → `ReadyToConnect` with the last known
    /// `ExitHealth`; otherwise fall back to `Routable` and rebuild from the
    /// next check. A fresh cycle is scheduled immediately.
    pub fn disconnecting(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if let RouteHealthState::Connecting { exit, .. } = &self.state {
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
        if let RouteHealthState::Connecting { tunnel_ping_rtt, .. } = &mut self.state {
            match rtt {
                Ok(rtt) => {
                    self.last_error = None;
                    self.consecutive_failures = 0;
                    *tunnel_ping_rtt = Some(rtt);
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

    /// Record an error message on this route without changing state.
    ///
    /// Used to surface transient failures (e.g. from Core-side operations
    /// like channel funding) in the CLI output. Ignored in `Unrecoverable`
    /// states to preserve the original failure reason.
    pub fn with_error(&mut self, err: String) {
        if matches!(self.state, RouteHealthState::Unrecoverable { .. }) {
            return;
        }
        self.last_error = Some(err);
    }

    fn exit_ref(&self) -> Option<&ExitHealth> {
        match &self.state {
            RouteHealthState::ReadyToConnect { exit } | RouteHealthState::Connecting { exit, .. } => Some(exit),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Health check spawn / cancel
// ---------------------------------------------------------------------------

/// Which sub-checks to include in a single health-check cycle.
///
/// Ping is always performed; version and exit-health are gated by
/// per-N-pings settings. This keeps the steady-state chatter on the exit
/// server down while still catching drift on a bounded schedule.
#[derive(Clone, Debug)]
struct CheckScope {
    version: bool,
    health: bool,
}

impl RouteHealth {
    /// Cancel any in-flight health check and schedule a new one after
    /// `delay`. The [`CheckScope`] is decided here from `check_cycle` and
    /// whether we are in `Connecting`. Called both by internal transitions
    /// and externally when a cycle completes.
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

        let is_connecting = matches!(self.state, RouteHealthState::Connecting { .. });
        let scope = if is_connecting {
            // During connecting, health runs every cycle at reduced frequency.
            CheckScope {
                version: false,
                health: true,
            }
        } else {
            CheckScope {
                version: cycle.is_multiple_of(intervals.version_every_n_pings),
                health: cycle.is_multiple_of(intervals.health_every_n_pings),
            }
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

    /// Cancel the running health-check task, if any, and replace the
    /// cancellation token so future spawns are independent. Safe to call
    /// when no check is running.
    pub fn cancel_health_check(&mut self) {
        self.health_check_cancel.cancel();
        self.health_check_cancel = CancellationToken::new();
    }
}

// ---------------------------------------------------------------------------
// Health check runner (async, runs in spawned task)
// ---------------------------------------------------------------------------

/// One health-check cycle, executed in a spawned task.
///
/// Opens a short-lived TCP bridge session to the exit, runs the sub-checks
/// selected by `scope` (version → exit health → ping), closes the session,
/// and sends a single [`HealthCheckOutcome`] back on `sender`. Any step
/// failing aborts the cycle and yields a `Failed` or `Unrecoverable`
/// outcome; only a fully successful run produces `Completed`.
async fn run_health_check(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    scope: &CheckScope,
    sender: &mpsc::Sender<Results>,
) {
    let id = destination.id.clone();
    let checked_at = SystemTime::now();
    tracing::info!(%id, %scope, "starting health check");
    let _ = sender
        .send(Results::HealthCheck {
            id: id.clone(),
            outcome: HealthCheckOutcome::Started { since: checked_at },
        })
        .await;

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

    // Step 1: Version check (when due)
    let socket_addr = session.bound_host;
    let timeout = options.timeouts.http;
    let client = reqwest::Client::new();
    let mut versions = None;
    if scope.version {
        let res_versions = gvpn_client::versions(&client, socket_addr, timeout).await;
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
                tracing::warn!(%id, ?err, "version check failed");
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
        let res_health = gvpn_client::health(&client, socket_addr, timeout).await;
        match res_health {
            Ok(h) => {
                tracing::debug!(%destination, health = %h, "received exit health status");
                health = Some(h);
            }
            Err(err) => {
                tracing::warn!(%id, ?err, "exit health request failed");
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

    // Step 3: Ping (always)
    let measure_rtt = Instant::now();
    let res_ping = gvpn_client::ping(&client, socket_addr, timeout).await;
    let ping_rtt = measure_rtt.elapsed();

    close_health_session(hopr, &session).await;

    match res_ping {
        Ok(_) => {
            tracing::debug!(%destination, ?ping_rtt, "exit ping successful");
            let _ = sender
                .send(Results::HealthCheck {
                    id,
                    outcome: HealthCheckOutcome::Completed {
                        checked_at,
                        versions,
                        ping_rtt: Some(ping_rtt),
                        health,
                    },
                })
                .await;
        }
        Err(err) => {
            tracing::warn!(%destination, error = %err, "exit ping failed");
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

/// Open a TCP bridge session to the exit dedicated to health checks.
///
/// Uses the configured bridge capabilities/target but disables SURB
/// management — the session is short-lived and not used for user traffic.
async fn open_health_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: None,
        ..Default::default()
    };
    tracing::debug!(%destination, "opening TCP session for health check");
    hopr.open_session(
        destination.address,
        options.sessions.bridge.target.clone(),
        None,
        None,
        cfg,
    )
    .await
}

/// Close a session opened by [`open_health_session`]. Errors are logged
/// and swallowed — a leaked session does not justify failing the check.
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
    /// Delay before the next retry after a failed cycle: linear growth in
    /// `consecutive_failures`, clamped at `MAX_INTERVAL_BETWEEN_FAILURES`.
    fn failure_backoff(&self) -> Duration {
        (FAILURE_INTERVAL + FAILURE_INTERVAL * self.consecutive_failures).min(MAX_INTERVAL_BETWEEN_FAILURES)
    }
}

// ---------------------------------------------------------------------------
// Free functions for Core
// ---------------------------------------------------------------------------

/// True iff at least one route is still waiting on peering. Core uses this
/// to pick a tighter polling interval for the connected-peers query while
/// any route is not yet routable.
pub fn any_needs_peers<'a>(healths: impl Iterator<Item = &'a RouteHealth>) -> bool {
    healths.into_iter().any(|rh| rh.needs_peer())
}

/// How many distinct outgoing channels are still required by the given
/// routes to make them all routable.
///
/// Routes with a fixed first-hop address count once per unique address.
/// `AnyChannel` routes collapse to a single slot since any one funded
/// channel satisfies them all. Routes that are already past funding, or
/// are peering-only, contribute nothing.
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

impl Display for CheckScope {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CheckScope {
                version: true,
                health: true,
            } => write!(f, "Scope(version,health,ping)"),
            CheckScope {
                version: true,
                health: false,
            } => write!(f, "Scope(version,ping)"),
            CheckScope {
                version: false,
                health: true,
            } => write!(f, "Scope(health,ping)"),
            CheckScope {
                version: false,
                health: false,
            } => write!(f, "Scope(ping)"),
        }
    }
}

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
            RouteHealthState::ReadyToConnect { exit } => match select_api_version(&exit.versions.versions) {
                Some(selected) => {
                    write!(f, "Ready to connect via API {selected}, exit health: {exit}")
                }
                // should never happen
                None => {
                    write!(f, "API version unsupported, exit health: {exit}")
                }
            },
            RouteHealthState::Connecting { exit, tunnel_ping_rtt } => match tunnel_ping_rtt {
                Some(rtt) => write!(
                    f,
                    "Connecting, tunnel ping RTT {:.2} s, exit: {exit}",
                    rtt.as_secs_f32()
                ),
                None => write!(f, "Connecting, tunnel ping pending, exit: {exit}"),
            },
        }
    }
}

impl Display for ExitHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} ago: ping RTT {:.2} s, {}, {}",
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
