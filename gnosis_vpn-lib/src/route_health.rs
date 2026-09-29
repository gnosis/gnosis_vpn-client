//! Per-destination routability from Core's graph walks; exit health lives in [`crate::probe`].
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

<<<<<<< HEAD
use crate::connection::destination::{Address, Destination, HopRouting};
use crate::connection::options::Options;
use crate::connection::options::surb_config_for;
use crate::core::runner::Results;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
=======
use edgli::hopr_lib::api::types::primitive::prelude::Address;

use crate::connection::destination::{Destination, ExitKey, HopRouting};
use crate::log_output;
use crate::probe::{Health, QuickProbeOutcome, Versions, select_api_version};
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
use crate::serde_utils;

/// Terminal failure modes. `NotAllowed` and `CannotOpenSession` need a config change; `IncompatibleApiVersion` an exit upgrade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UnrecoverableReason {
    /// 0-hop without `--allow-insecure`, or 2+ hops without `--allow-experimental`.
    NotAllowed,
    /// The exit server only offers API versions we do not support.
    IncompatibleApiVersion { server_versions: Vec<String> },
    /// No session can be opened with this client's session config, so every retry aborts the same way.
    CannotOpenSession { error: String },
}

/// Also the wire format shown by the CLI, so variant names are user-visible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state")]
pub enum RouteHealthState {
    Unrecoverable {
        reason: UnrecoverableReason,
    },
    /// The graph holds no plannable path to the exit right now.
    NotRoutable,
    /// A path exists; connecting may proceed.
    Routable,
}

/// The last graph walk for this exit; also the wire format shown by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "found")]
pub enum RouteWalk {
    /// The exit's chain address maps to no packet key - it never announced itself.
    NotAnnounced {
        #[serde(with = "serde_utils::system_time")]
        walked_at: SystemTime,
    },
    /// The selector accepted no path over this destination's hop count.
    NoPath {
        #[serde(with = "serde_utils::system_time")]
        walked_at: SystemTime,
    },
    Paths {
        #[serde(with = "serde_utils::system_time")]
        walked_at: SystemTime,
        /// Capped at the planner's own `max_cached_paths`.
        count: usize,
        /// Distinct first relays among them; 1 means a single point of failure.
        distinct_first_relays: usize,
        /// Relays of the best-valued path, in path order; empty on a 0-hop route.
        #[serde(with = "serde_utils::addresses")]
        best_relays: Vec<Address>,
        /// The best path's value in (0.0, 1.0]; higher is better.
        best_value: f64,
    },
}

/// What one quick probe measured on the exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickProbeCheck {
    #[serde(with = "serde_utils::system_time")]
    pub checked_at: SystemTime,
    pub versions: Versions,
    /// The API version this client selected from `versions`; None means incompatible.
    pub api_version: Option<String>,
    pub load: Health,
    /// Round-trip of the status request, so it carries the exit's status-generation time too.
    #[serde(with = "serde_utils::duration_ms")]
    pub status_rtt: Duration,
}

/// The last `quickprobe` of this exit; also the wire format shown by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum QuickProbeState {
    Checking {
        #[serde(with = "serde_utils::system_time")]
        since: SystemTime,
        /// The result this check refreshes; kept so a re-check does not blank the exit's data.
        last: Option<QuickProbeCheck>,
    },
    Checked(QuickProbeCheck),
    Failed {
        #[serde(with = "serde_utils::system_time")]
        checked_at: SystemTime,
        error: String,
    },
}

pub(crate) struct RouteHealth {
<<<<<<< HEAD
    id: String,
    static_need: StaticNeed,
=======
    key: ExitKey,
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
    state: RouteHealthState,
    last_error: Option<String>,
    last_walk: Option<RouteWalk>,
    quick_probe: Option<QuickProbeState>,
}

impl RouteHealth {
    pub(crate) fn new(dest: &Destination, allow_insecure: bool, allow_experimental: bool) -> Self {
        Self {
<<<<<<< HEAD
            id: dest.id.clone(),
            static_need,
            state,
            health_check_cancel,
            cancel_on_shutdown,
            check_cycle: 0,
            checking_since: None,
            exit_failures: 0,
            exit_last_error: None,
            tunnel_ping_failures: 0,
            tunnel_ping_last_error: None,
=======
            key: dest.key(),
            state: derive_initial_state(&dest.routing, allow_insecure, allow_experimental),
            last_error: None,
            last_walk: None,
            quick_probe: None,
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
        }
    }

    pub(crate) fn walk(&self) -> Option<&RouteWalk> {
        self.last_walk.as_ref()
    }

    pub(crate) fn quick_probe(&self) -> Option<&QuickProbeState> {
        self.quick_probe.as_ref()
    }

    pub(crate) fn is_quick_probing(&self) -> bool {
        matches!(self.quick_probe, Some(QuickProbeState::Checking { .. }))
    }

    /// Claims the exit for one quick probe; false while another is still running against it.
    pub(crate) fn start_quick_probe(&mut self, now: SystemTime) -> bool {
        if self.is_quick_probing() {
            return false;
        }
        // A re-check must not blank the exit it refreshes; `checked_at` still says how stale it is.
        let last = match self.quick_probe.take() {
            Some(QuickProbeState::Checked(check)) => Some(check),
            _ => None,
        };
        self.quick_probe = Some(QuickProbeState::Checking { since: now, last });
        true
    }

    /// Records what the quick probe found; a compatible API also unlatches the route like any probe does.
    pub(crate) fn apply_quick_probe(&mut self, outcome: &Result<QuickProbeOutcome, String>, now: SystemTime) {
        self.quick_probe = Some(match outcome {
            Ok(found) => {
                self.apply_api_versions(&found.versions.versions);
                QuickProbeState::Checked(QuickProbeCheck {
                    checked_at: now,
                    versions: found.versions.clone(),
                    api_version: found.api_version.clone(),
                    load: found.health.clone(),
                    status_rtt: found.status_rtt,
                })
            }
            Err(error) => QuickProbeState::Failed {
                checked_at: now,
                error: error.clone(),
            },
        });
    }

    pub(crate) fn state(&self) -> &RouteHealthState {
        &self.state
    }

    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

<<<<<<< HEAD
    pub(crate) fn checking_since(&self) -> Option<SystemTime> {
        self.checking_since
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.exit_failures
    }

    pub(crate) fn needs_peer(&self) -> bool {
        matches!(self.state, RouteHealthState::NeedsPeering { .. })
    }

    pub(crate) fn needs_channel(&self) -> bool {
        matches!(self.state, RouteHealthState::NeedsChannel)
    }

    pub(crate) fn ready_to_connect(&self) -> Option<ExitHealth> {
        match &self.state {
            RouteHealthState::ReadyToConnect { exit } => Some(exit.clone()),
            _ => None,
        }
    }

    pub(crate) fn is_ready_to_connect(&self) -> bool {
        matches!(self.state, RouteHealthState::ReadyToConnect { .. })
    }

    /// Returns the cached exit health from either `ReadyToConnect` or `Connecting` state.
    /// Used by force-reconnect to reuse the last known good health without going through a
    /// full disconnect/reconnect cycle.
    pub(crate) fn current_exit_health(&self) -> Option<ExitHealth> {
        match &self.state {
            RouteHealthState::ReadyToConnect { exit } | RouteHealthState::Connecting { exit, .. } => Some(exit.clone()),
            _ => None,
        }
=======
    pub(crate) fn is_routable(&self) -> bool {
        matches!(self.state, RouteHealthState::Routable)
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
    }

    pub fn is_unrecoverable(&self) -> bool {
        matches!(self.state, RouteHealthState::Unrecoverable { .. })
    }

<<<<<<< HEAD
// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

impl RouteHealth {
    /// Apply a fresh snapshot of connected peer addresses.
    ///
    /// Advances or regresses the state depending on whether the route's
    /// [`StaticNeed`] is currently satisfied. When a route that previously had
    /// a channel loses its peer we transition back to
    /// `NeedsPeering { has_channel: true }` so that re-peering skips straight
    /// to `Routable`. When the route first becomes routable we spawn the
    /// initial health check.
    ///
    pub(crate) fn peers(
        &mut self,
        addresses: &HashSet<Address>,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
        initial_delay: Duration,
    ) {
        let is_peered = match &self.static_need {
            // 0-hop: destination must be a direct transport peer.
            StaticNeed::Peering(addr) => addresses.contains(addr),
            // 1+ hop: any connected relay can carry packets; the exit node
            // itself does not need to be a direct transport peer.
            StaticNeed::AnyChannel => !addresses.is_empty(),
        };

        match &self.state {
            RouteHealthState::NeedsPeering { has_channel } => {
                if !is_peered {
                    return;
                }
                // 0-hop routes never need a channel. For 1+ hop routes, skip
                // NeedsChannel if one already existed (transient peer flap).
                let skip_channel_wait = matches!(self.static_need, StaticNeed::Peering(_)) || *has_channel;
                if skip_channel_wait {
                    tracing::debug!(destination = %self.id, "peers available → Routable");
                    self.state = RouteHealthState::Routable;
                    self.spawn_health_check(initial_delay, hopr, dest, options, sender);
                } else {
                    tracing::debug!(destination = %self.id, "peers available → NeedsChannel");
                    self.state = RouteHealthState::NeedsChannel;
                }
            }
            RouteHealthState::NeedsChannel => {
                if !is_peered {
                    tracing::debug!(destination = %self.id, "peers lost → NeedsPeering");
                    // No channel was ever seen, so has_channel stays false.
                    self.state = RouteHealthState::NeedsPeering { has_channel: false };
                }
=======
    /// A config-level refusal; only a config change lifts it, so probing it is pointless.
    pub(crate) fn is_not_allowed(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::NotAllowed
            }
        )
    }

    /// The exit's own version latch; an upgrade lifts it, so it stays worth re-checking.
    pub(crate) fn is_incompatible_api(&self) -> bool {
        matches!(
            self.state,
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::IncompatibleApiVersion { .. }
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
            }
        )
    }

    /// Record what the walk found. Returns true iff the route just became routable.
    pub(crate) fn apply_walk(&mut self, walk: RouteWalk) -> bool {
        let routable = matches!(walk, RouteWalk::Paths { .. });
        self.last_walk = Some(walk);
        self.last_error = None;
        self.set_routable(routable)
    }

    /// Apply a graph walk result. Returns true iff the route just became routable.
    pub(crate) fn set_routable(&mut self, routable: bool) -> bool {
        let next = if routable {
            RouteHealthState::Routable
<<<<<<< HEAD
            | RouteHealthState::ReadyToConnect { .. }
            | RouteHealthState::Connecting { .. } => {
                if !is_peered {
                    tracing::debug!(destination = %self.id, state = ?self.state, "peers lost → NeedsPeering");
                    self.cancel_health_check();
                    self.checking_since = None;
                    self.check_cycle = 0;
                    self.exit_failures = 0;
                    self.tunnel_ping_failures = 0;
                    // 0-hop routes never go through a channel wait, so has_channel
                    // stays false. 1+ hop routes reached Routable via a channel, so
                    // has_channel: true lets re-peering skip straight back to Routable.
                    let has_channel = matches!(self.static_need, StaticNeed::AnyChannel);
                    self.state = RouteHealthState::NeedsPeering { has_channel };
                }
            }
            RouteHealthState::Unrecoverable { .. } => {}
        }
    }

    /// Notify that at least one outgoing channel exists.
    ///
    /// Routes in `NeedsChannel` become routable and schedule their first
    /// health check immediately. No-op for routes in any other state.
    pub(crate) fn any_channel_available(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if !matches!(self.state, RouteHealthState::NeedsChannel) {
            return;
        }
        tracing::debug!(destination = %self.id, "channel available → Routable");
        self.state = RouteHealthState::Routable;
        self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
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
    ///   ping interval; failure schedules with a linear backoff.
    ///
    /// Outcomes that arrive when the state is no longer `Routable` /
    /// `ReadyToConnect` / `Connecting` (e.g. because peering was lost)
    /// are dropped.
    pub(crate) fn health_check_result(
        &mut self,
        outcome: HealthCheckOutcome,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        match outcome {
            HealthCheckOutcome::Started { since } => {
                self.checking_since = Some(since);
            }
            HealthCheckOutcome::Unrecoverable { reason } => {
                tracing::debug!(destination = %self.id, ?reason, "health check → Unrecoverable");
                self.checking_since = None;
                self.state = RouteHealthState::Unrecoverable { reason };
            }
            HealthCheckOutcome::Failed { checked_at, error } => {
                tracing::debug!(destination = %self.id, %error, failures = self.exit_failures + 1, "health check failed");
                self.checking_since = None;
                self.exit_failures += 1;
                self.exit_last_error = Some(error);
                // drop to routable from ready-to-connect, stay in connecting when connecting
                self.state = match &self.state {
                    RouteHealthState::ReadyToConnect { .. } => {
                        self.check_cycle = 0;
                        RouteHealthState::Routable
                    }
                    RouteHealthState::Connecting { exit, tunnel_ping_rtt } => RouteHealthState::Connecting {
                        exit: ExitHealth {
                            checked_at,
                            versions: exit.versions.clone(),
                            ping_rtt: exit.ping_rtt,
                            health: exit.health.clone(),
                        },
                        tunnel_ping_rtt: *tunnel_ping_rtt,
                    },
                    s => s.clone(),
                };
                // spawn from linear failure backoff
                let delay = self.failure_backoff();
                self.spawn_health_check(delay, hopr, dest, options, sender);
            }

            HealthCheckOutcome::Completed {
                checked_at,
                versions,
                ping_rtt,
                health,
            } => {
                self.checking_since = None;
                self.exit_failures = 0;
                self.exit_last_error = None;
                self.check_cycle = self.check_cycle.wrapping_add(1);
                self.state = match &self.state {
                    RouteHealthState::Connecting { exit, tunnel_ping_rtt } => RouteHealthState::Connecting {
                        exit: ExitHealth {
                            checked_at,
                            versions: versions.unwrap_or(exit.versions.clone()),
                            ping_rtt: ping_rtt.unwrap_or(exit.ping_rtt),
                            health: health.unwrap_or(exit.health.clone()),
                        },
                        tunnel_ping_rtt: *tunnel_ping_rtt,
                    },
                    RouteHealthState::ReadyToConnect { exit } => RouteHealthState::ReadyToConnect {
                        exit: ExitHealth {
                            checked_at,
                            versions: versions.unwrap_or(exit.versions.clone()),
                            ping_rtt: ping_rtt.unwrap_or(exit.ping_rtt),
                            health: health.unwrap_or(exit.health.clone()),
                        },
                    },
                    _ => match (versions, ping_rtt, health) {
                        (Some(versions), Some(ping_rtt), Some(health)) => {
                            tracing::debug!(destination = %self.id, "health check completed → ReadyToConnect");
                            RouteHealthState::ReadyToConnect {
                                exit: ExitHealth {
                                    checked_at,
                                    versions,
                                    ping_rtt,
                                    health,
                                },
                            }
                        }
                        _ => {
                            tracing::warn!(destination = %self.id, state = ?self.state, "received unexpected outcome - setting to routable");
                            RouteHealthState::Routable
                        }
                    },
                };

                let delay = match self.state {
                    RouteHealthState::Connecting { .. } => {
                        // during connecting state skip all in between pings
                        let intervals = &options.health_check_intervals;
                        intervals.ping * intervals.health_every_n_pings
                    }
                    _ => options.health_check_intervals.ping,
                };

                self.spawn_health_check(delay, hopr, dest, options, sender);
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
    pub(crate) fn connecting(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        exit: ExitHealth,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        self.checking_since = None;
        self.exit_failures = 0;
        self.exit_last_error = None;
        self.tunnel_ping_failures = 0;
        self.tunnel_ping_last_error = None;
        tracing::debug!(destination = %self.id, "→ Connecting");
        self.state = RouteHealthState::Connecting {
            exit,
            tunnel_ping_rtt: None,
        };
        let delay = options.health_check_intervals.ping;
        self.spawn_health_check(delay, hopr, dest, options, sender);
    }

    /// Leave `Connecting` and resume normal health checking.
    ///
    /// The resulting state depends on whether the route is still considered
    /// healthy: no recent failures → `ReadyToConnect` with the last known
    /// `ExitHealth`; otherwise fall back to `Routable` and rebuild from the
    /// next check. A fresh cycle is scheduled immediately.
    pub(crate) fn disconnecting(
        &mut self,
        hopr: &Arc<Hopr>,
        dest: &Destination,
        options: &Options,
        sender: &mpsc::Sender<Results>,
    ) {
        if let RouteHealthState::Connecting { exit, .. } = &self.state {
            let exit = exit.clone();
            if self.exit_failures == 0 {
                tracing::debug!(destination = %self.id, "disconnecting → ReadyToConnect");
                self.state = RouteHealthState::ReadyToConnect { exit };
            } else {
                tracing::debug!(destination = %self.id, failures = self.exit_failures, "disconnecting → Routable");
                self.check_cycle = 0;
                self.state = RouteHealthState::Routable;
            }
            self.spawn_health_check(Duration::ZERO, hopr, dest, options, sender);
        }
    }

    /// Update exit health from a tunnel ping result. Returns the tunnel ping
    /// failure count after applying this result. On success the `ping_rtt` is
    /// refreshed with the new measurement. On failure the exit data is
    /// preserved and `tunnel_ping_failures` is incremented.
    pub(crate) fn tunnel_ping_result(&mut self, rtt: Result<Duration, String>) -> u32 {
        if let RouteHealthState::Connecting { tunnel_ping_rtt, .. } = &mut self.state {
            match rtt {
                Ok(rtt) => {
                    self.tunnel_ping_failures = 0;
                    self.tunnel_ping_last_error = None;
                    *tunnel_ping_rtt = Some(rtt);
                    0
                }
                Err(err) => {
                    self.tunnel_ping_failures += 1;
                    self.tunnel_ping_last_error = Some(err);
                    self.tunnel_ping_failures
                }
            }
=======
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
        } else {
            RouteHealthState::NotRoutable
        };
        match self.state {
            RouteHealthState::Unrecoverable { .. } => false,
            _ if self.state == next => false,
            _ => {
                tracing::debug!(destination = %self.key, from = %self.state, to = %next, "routability changed");
                self.state = next;
                routable
            }
        }
    }

    /// One rule for every probe kind: an exit's advertised API versions latch or unlatch the route.
    pub(crate) fn apply_api_versions(&mut self, server_versions: &[String]) {
        match select_api_version(server_versions) {
            Some(_) => self.clear_incompatible(),
            None => self.set_incompatible(server_versions.to_vec()),
        }
    }

    /// Latch a route whose probe can never open a session; nothing retries it, so a waiting target must see why.
    pub(crate) fn set_cannot_open_session(&mut self, error: String) {
        if self.is_unrecoverable() {
            return;
        }
        tracing::warn!(destination = %self.key, %error, "probe cannot open a session with this config");
        self.state = RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::CannotOpenSession { error },
        };
    }

    /// Latch on an exit that speaks no API version we support. `NotAllowed` keeps precedence.
    fn set_incompatible(&mut self, server_versions: Vec<String>) {
        if self.is_unrecoverable() {
            return;
        }
        tracing::warn!(destination = %self.key, ?server_versions, "exit offers no compatible API version");
        self.state = RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::IncompatibleApiVersion { server_versions },
        };
    }

    /// An exit upgrade unlatches the route; the next graph walk decides routability again.
    fn clear_incompatible(&mut self) {
        if self.is_incompatible_api() {
            tracing::info!(destination = %self.key, "exit API version compatible again");
            self.state = RouteHealthState::NotRoutable;
        }
    }

    /// Surface a transient Core-side failure in the CLI; kept out of `Unrecoverable` to preserve its reason.
    pub(crate) fn with_error(&mut self, err: String) {
        if self.is_unrecoverable() {
            return;
        }
        self.last_error = Some(err);
    }
}

<<<<<<< HEAD
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
    /// `delay`. The check scope (which fields to fetch) is decided here from `check_cycle` and
    /// whether we are in `Connecting`. Called both by internal transitions
    /// and externally when a cycle completes.
    fn spawn_health_check(
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
        // during connecting we always only run health checks. the interval was increased
        // accordingly on task spawn
        let scope = if is_connecting {
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
        let hopr = hopr.clone();
        let dest = dest.clone();
        let options = options.clone();
        let sender = sender.clone();

        tokio::spawn(async move {
            token
                .run_until_cancelled(async {
                    time::sleep(jitter(delay)).await;
                    run_health_check(hopr, &dest, &options, &scope, &sender).await;
                })
                .await;
        });
    }

    /// Cancel the running health-check task, if any, and replace the
    /// cancellation token so future spawns are independent. Safe to call
    /// when no check is running.
    fn cancel_health_check(&mut self) {
        self.health_check_cancel.cancel();
        self.health_check_cancel = self.cancel_on_shutdown.child_token();
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
    hopr: Arc<Hopr>,
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

    let res_session = HealthSession::open(hopr, destination, options).await;
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
    // From here on, early returns drop `session`, whose Drop detaches a
    // close task — so we do not leak the TCP bridge even if the surrounding
    // future is cancelled via `tokio::select!`.
    let socket_addr = session.meta.bound_host;
    let timeout = options.timeouts.http;
    let client = reqwest::Client::new();
    let mut versions = None;
    if scope.version {
        let res_versions = gvpn_client::versions(&client, socket_addr, timeout).await;
        match res_versions {
            Ok(v) => {
                if select_api_version(&v.versions).is_none() {
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

    session.close().await;

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

/// RAII guard for the short-lived TCP bridge session used during a
/// health check.
///
/// Guarantees the session is closed even if the surrounding future is
/// dropped — e.g. cancelled via `tokio::select!` on the shutdown or
/// per-check cancellation token. The success path calls
/// [`HealthSession::close`] to await the close inline so the
/// `Completed` outcome is reported only after cleanup. Any other path —
/// early `return` on error or future cancellation — falls through to
/// `Drop`, which detaches a close task on the tokio runtime so the exit
/// port is not leaked.
struct HealthSession {
    hopr: Arc<Hopr>,
    meta: SessionClientMetadata,
    closed: bool,
}

impl HealthSession {
    /// Open a TCP bridge session to the exit dedicated to health checks.
    ///
    /// Uses the configured bridge capabilities/target and applies the health-check SURB settings —
    /// the session is short-lived and not used for user traffic.
    async fn open(hopr: Arc<Hopr>, destination: &Destination, options: &Options) -> Result<Self, HoprError> {
        let health_surb =
            surb_config_for(&options.surb_balancing.health_check).map_err(|e| HoprError::Session(e.to_string()))?;
        let cfg = HoprSessionClientConfig {
            capabilities: options.sessions.bridge.capabilities,
            forward_path: destination.routing,
            return_path: destination.routing,
            always_max_out_surbs: health_surb.always_max_out_surbs,
            surb_management: health_surb.management,
            ..Default::default()
        };
        tracing::debug!(%destination, "opening TCP session for health check");
        let meta = hopr
            .open_session(
                destination.address,
                options.sessions.bridge.target.clone(),
                None,
                None,
                cfg,
            )
            .await?;
        Ok(Self {
            hopr,
            meta,
            closed: false,
        })
    }

    /// Close the session, awaiting completion. Disarms the `Drop` guard.
    async fn close(mut self) {
        close_health_session(&self.hopr, &self.meta).await;
        self.closed = true;
    }
}

impl Drop for HealthSession {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        tracing::debug!("health session dropped without explicit close, spawning detached close task");
        // Explicit `close()` never ran — detach a close task so the exit
        // port is not leaked. Fire and forget; errors are logged inside
        // `close_health_session`.
        let hopr = self.hopr.clone();
        let meta = self.meta.clone();
        tokio::spawn(async move {
            close_health_session(&hopr, &meta).await;
        });
    }
}

/// Close a session opened by [`HealthSession::open`]. Errors are logged
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
    /// Delay before the next retry after a failed exit-health cycle.
    ///
    /// The first `GRAPH_WARMUP_RETRY_COUNT` failures use a short
    /// `GRAPH_WARMUP_RETRY_INTERVAL` so the retry fires after the HOPR
    /// transport heartbeat has had time to probe relay peers and populate
    /// `is_connected()` in the channel graph (typically within one probe
    /// cycle, ~3 s).  Subsequent failures fall through to the normal linear
    /// backoff clamped at `MAX_INTERVAL_BETWEEN_FAILURES`.
    fn failure_backoff(&self) -> Duration {
        if self.exit_failures <= GRAPH_WARMUP_RETRY_COUNT {
            GRAPH_WARMUP_RETRY_INTERVAL
        } else {
            let normal_failures = self.exit_failures - GRAPH_WARMUP_RETRY_COUNT;
            FAILURE_INTERVAL
                .saturating_mul(normal_failures)
                .min(MAX_INTERVAL_BETWEEN_FAILURES)
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions for Core
// ---------------------------------------------------------------------------

/// True iff at least one route is still waiting on peering. Core uses this
/// to pick a tighter polling interval for the connected-peers query while
/// any route is not yet routable.
pub(crate) fn any_needs_peers<'a>(healths: impl Iterator<Item = &'a RouteHealth>) -> bool {
    healths.into_iter().any(|rh| rh.needs_peer())
}

/// True iff at least one route is in `NeedsChannel`. Core uses this to pick
/// a tighter balances polling interval until a channel appears.
pub(crate) fn any_needs_channel<'a>(healths: impl Iterator<Item = &'a RouteHealth>) -> bool {
    healths.into_iter().any(|rh| rh.needs_channel())
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
=======
fn derive_initial_state(routing: &HopRouting, allow_insecure: bool, allow_experimental: bool) -> RouteHealthState {
    let hops = routing.hop_count();
    let insecure_without_optin = hops == 0 && !allow_insecure;
    let experimental_without_optin = hops > 1 && !allow_experimental;
    if insecure_without_optin || experimental_without_optin {
        RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::NotAllowed,
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
        }
    } else {
        RouteHealthState::NotRoutable
    }
}

impl Display for UnrecoverableReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UnrecoverableReason::NotAllowed => write!(
                f,
                "routing mode not allowed; use --allow-insecure for 0-hop or --allow-experimental for 2+ hops"
            ),
            UnrecoverableReason::IncompatibleApiVersion { server_versions } => write!(
                f,
                "exit server offers no compatible API version (server offers: {})",
                server_versions.join(", ")
            ),
            UnrecoverableReason::CannotOpenSession { error } => {
                write!(f, "cannot open a session with this connection config: {error}")
            }
        }
    }
}

impl Display for RouteWalk {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteWalk::NotAnnounced { walked_at } => {
                write!(
                    f,
                    "exit not announced on chain - walked {} ago",
                    log_output::elapsed(walked_at)
                )
            }
            RouteWalk::NoPath { walked_at } => {
                write!(f, "no path found - walked {} ago", log_output::elapsed(walked_at))
            }
            RouteWalk::Paths {
                walked_at,
                count,
                distinct_first_relays,
                best_relays,
                best_value,
            } => {
                let plural = if *count == 1 { "" } else { "s" };
                write!(f, "{count} path{plural}, ")?;
                // A 0-hop route has no relays, so there is no diversity or "via" to report.
                if best_relays.is_empty() {
                    write!(f, "direct")?;
                } else {
                    let via = best_relays
                        .iter()
                        .map(log_output::address)
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    write!(f, "{distinct_first_relays} distinct first relays, best via {via}")?;
                }
                write!(
                    f,
                    " (value {best_value:.3}) - walked {} ago",
                    log_output::elapsed(walked_at)
                )
            }
        }
    }
}

impl Display for QuickProbeCheck {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let QuickProbeCheck {
            checked_at,
            versions,
            api_version,
            load,
            status_rtt,
        } = self;
        write!(
            f,
            "checked {} ago - status RTT {:.2} s, {load}",
            log_output::elapsed(checked_at),
            status_rtt.as_secs_f32()
        )?;
        match api_version {
            Some(api) => write!(f, ", API {api} ({versions})"),
            None => write!(f, ", no compatible API ({versions})"),
        }
    }
}

impl Display for QuickProbeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            QuickProbeState::Checking { since, last } => {
                write!(f, "checking (since {})", log_output::elapsed(since))?;
                match last {
                    Some(check) => write!(f, " - last {check}"),
                    None => Ok(()),
                }
            }
            QuickProbeState::Checked(check) => write!(f, "{check}"),
            QuickProbeState::Failed { checked_at, error } => {
                write!(f, "failed {} ago: {error}", log_output::elapsed(checked_at))
            }
        }
    }
}

impl Display for RouteHealthState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RouteHealthState::Unrecoverable { reason } => write!(f, "Unrecoverable: {reason}"),
            RouteHealthState::NotRoutable => write!(f, "Not routable - no path in the network graph"),
            RouteHealthState::Routable => write!(f, "Routable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Address, DestinationSource};

<<<<<<< HEAD
    fn addr(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    /// Mirror the `is_peered` logic from `peers()` so we can test it without
    /// constructing heavyweight `Arc<Hopr>` / `Options` / channel types.
    fn is_peered(need: &StaticNeed, connected: &HashSet<Address>) -> bool {
        match need {
            StaticNeed::Peering(a) => connected.contains(a),
            StaticNeed::AnyChannel => !connected.is_empty(),
        }
    }

    // --- derive_static_need ---

    #[test]
    fn zero_hop_routing_yields_peering() {
        let dest = addr(1);
        let routing = HopRouting::try_from(0).expect("0-hop is valid");
        assert_eq!(derive_static_need(&routing, dest), StaticNeed::Peering(dest));
    }

    #[test]
    fn one_hop_routing_yields_any_channel() {
        let dest = addr(2);
        let routing = HopRouting::try_from(1).expect("1-hop is valid");
        assert_eq!(derive_static_need(&routing, dest), StaticNeed::AnyChannel);
    }

    // --- derive_initial_state ---

    #[test]
    fn zero_hop_without_allow_insecure_is_unrecoverable() {
        let routing = HopRouting::try_from(0).unwrap();
        assert!(matches!(
            derive_initial_state(&routing, false, false),
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::NotAllowed
            }
        ));
    }

    #[test]
    fn zero_hop_with_allow_insecure_is_allowed() {
        let routing = HopRouting::try_from(0).unwrap();
        assert!(matches!(
            derive_initial_state(&routing, true, false),
            RouteHealthState::NeedsPeering { .. }
        ));
    }

    #[test]
    fn one_hop_always_allowed() {
        let routing = HopRouting::try_from(1).unwrap();
        assert!(matches!(
            derive_initial_state(&routing, false, false),
            RouteHealthState::NeedsPeering { .. }
        ));
    }

    #[test]
    fn multi_hop_without_allow_experimental_is_unrecoverable() {
        let routing = HopRouting::try_from(2).unwrap();
        assert!(matches!(
            derive_initial_state(&routing, false, false),
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::NotAllowed
            }
        ));
    }

    #[test]
    fn multi_hop_with_allow_experimental_is_allowed() {
        let routing = HopRouting::try_from(2).unwrap();
        assert!(matches!(
            derive_initial_state(&routing, false, true),
            RouteHealthState::NeedsPeering { .. }
        ));
    }

    // --- is_peered for AnyChannel routes ---

    #[test]
    fn any_channel_is_peered_when_any_relay_is_connected() {
        let relay = addr(20);
        let need = StaticNeed::AnyChannel;

        let mut peers = HashSet::new();
        peers.insert(relay);

        assert!(is_peered(&need, &peers));
    }

    #[test]
    fn any_channel_is_not_peered_when_no_peers_at_all() {
        assert!(!is_peered(&StaticNeed::AnyChannel, &HashSet::new()));
    }

    // --- failure_backoff ---

    fn backoff_at(failures: u32) -> Duration {
        use crate::connection::destination::{Destination, HopRouting};
        use tokio_util::sync::CancellationToken;
        let dest = Destination::new(
=======
    fn destination(hops: usize) -> Destination {
        Destination::new(
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
            "test".to_string(),
            Address::from([1u8; 20]),
            HopRouting::try_from(hops).unwrap(),
            Default::default(),
<<<<<<< HEAD
        );
        let mut rh = RouteHealth::new(&dest, false, false, CancellationToken::new());
        rh.exit_failures = failures;
        rh.failure_backoff()
    }

    #[test]
    fn failure_backoff_uses_warmup_interval_for_first_failures() {
        for n in 1..=GRAPH_WARMUP_RETRY_COUNT {
            assert_eq!(
                backoff_at(n),
                GRAPH_WARMUP_RETRY_INTERVAL,
                "failure {n} should use warmup interval"
            );
=======
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        )
    }

    fn outcome(server_versions: &[&str]) -> QuickProbeOutcome {
        let versions = Versions {
            versions: server_versions.iter().map(|v| v.to_string()).collect(),
            latest: server_versions.last().unwrap_or(&"").to_string(),
        };
        QuickProbeOutcome {
            api_version: select_api_version(&versions.versions).map(str::to_owned),
            versions,
            health: Health {
                slots: crate::probe::Slots {
                    total: 10,
                    available: 9,
                    connected: 1,
                },
                load_avg: crate::probe::LoadAvg {
                    one: 0.1,
                    five: 0.1,
                    fifteen: 0.1,
                    nproc: 4,
                },
            },
            status_rtt: Duration::from_millis(120),
>>>>>>> 14212f2 (feat(route_health): expose graph data, path probing now API triggered (#843))
        }
    }

    fn walk_with_paths(now: SystemTime) -> RouteWalk {
        RouteWalk::Paths {
            walked_at: now,
            count: 3,
            distinct_first_relays: 2,
            best_relays: vec![Address::from([2u8; 20])],
            best_value: 0.75,
        }
    }

    fn not_allowed(state: &RouteHealthState) -> bool {
        matches!(
            state,
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::NotAllowed
            }
        )
    }

    #[test]
    fn zero_hop_without_allow_insecure_is_unrecoverable() {
        assert!(not_allowed(RouteHealth::new(&destination(0), false, false).state()));
    }

    #[test]
    fn zero_hop_with_allow_insecure_starts_not_routable() {
        assert_eq!(
            *RouteHealth::new(&destination(0), true, false).state(),
            RouteHealthState::NotRoutable
        );
    }

    #[test]
    fn a_probe_that_cannot_open_a_session_latches_the_route() {
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.set_cannot_open_session("surb buffer too small".to_string());
        assert!(rh.is_unrecoverable());
        assert!(!rh.is_routable());
    }

    #[test]
    fn a_not_allowed_route_keeps_its_reason_over_a_failed_open() {
        let mut rh = RouteHealth::new(&destination(0), false, false);
        rh.set_cannot_open_session("surb buffer too small".to_string());
        assert!(not_allowed(rh.state()));
    }

    #[test]
    fn one_hop_always_allowed() {
        assert_eq!(
            *RouteHealth::new(&destination(1), false, false).state(),
            RouteHealthState::NotRoutable
        );
    }

    #[test]
    fn multi_hop_needs_allow_experimental() {
        assert!(not_allowed(RouteHealth::new(&destination(2), false, false).state()));
        assert_eq!(
            *RouteHealth::new(&destination(2), false, true).state(),
            RouteHealthState::NotRoutable
        );
    }

    #[test]
    fn set_routable_reports_only_the_transition_to_routable() {
        let mut rh = RouteHealth::new(&destination(1), false, false);
        assert!(rh.set_routable(true));
        assert!(!rh.set_routable(true), "already routable");
        assert!(
            !rh.set_routable(false),
            "losing the route is not a transition to routable"
        );
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);
    }

    #[test]
    fn apply_walk_reports_routable_only_when_the_walk_found_paths() {
        let now = SystemTime::now();
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.with_error("graph walk failed".to_string());

        assert!(rh.apply_walk(walk_with_paths(now)));
        assert!(rh.is_routable());
        assert!(rh.last_error().is_none(), "a walk that ran clears the previous failure");
        assert!(matches!(rh.walk(), Some(RouteWalk::Paths { count: 3, .. })));

        assert!(!rh.apply_walk(RouteWalk::NoPath { walked_at: now }));
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);

        assert!(!rh.apply_walk(RouteWalk::NotAnnounced { walked_at: now }));
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);
        assert!(matches!(rh.walk(), Some(RouteWalk::NotAnnounced { .. })));
    }

    #[test]
    fn apply_walk_never_unlatches_unrecoverable() {
        let mut rh = RouteHealth::new(&destination(0), false, false);
        assert!(!rh.apply_walk(walk_with_paths(SystemTime::now())));
        assert!(
            not_allowed(rh.state()),
            "the walk still gets recorded, the verdict does not change"
        );
        assert!(rh.walk().is_some());
    }

    #[test]
    fn set_routable_never_unlatches_unrecoverable() {
        let mut rh = RouteHealth::new(&destination(0), false, false);
        assert!(!rh.set_routable(true));
        assert!(not_allowed(rh.state()));
    }

    #[test]
    fn api_versions_latch_and_unlatch_only_an_incompatible_api_version() {
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.set_routable(true);
        rh.apply_api_versions(&["v99".to_string()]);
        assert!(rh.is_unrecoverable());
        rh.apply_api_versions(&["v1".to_string()]);
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);

        let mut latched = RouteHealth::new(&destination(0), false, false);
        latched.apply_api_versions(&["v99".to_string()]);
        latched.apply_api_versions(&["v1".to_string()]);
        assert!(not_allowed(latched.state()), "NotAllowed keeps precedence");
    }

    #[test]
    fn only_the_config_latch_blocks_a_probe() {
        let routable = RouteHealth::new(&destination(1), false, false);
        assert!(!routable.is_not_allowed());
        assert!(!routable.is_incompatible_api());

        let insecure = RouteHealth::new(&destination(0), false, false);
        assert!(insecure.is_not_allowed());
        assert!(!insecure.is_incompatible_api());

        let mut incompatible = RouteHealth::new(&destination(1), false, false);
        incompatible.apply_api_versions(&["v99".to_string()]);
        assert!(!incompatible.is_not_allowed(), "an upgrade can still lift this one");
        assert!(incompatible.is_incompatible_api());
    }

    #[test]
    fn quick_probe_is_claimed_once_until_it_reports() {
        let now = SystemTime::now();
        let mut rh = RouteHealth::new(&destination(1), false, false);
        assert!(rh.quick_probe().is_none());
        assert!(!rh.is_quick_probing());
        assert!(rh.start_quick_probe(now));
        assert!(!rh.start_quick_probe(now), "still checking");
        assert!(rh.is_quick_probing());
        assert!(matches!(
            rh.quick_probe(),
            Some(QuickProbeState::Checking { last: None, .. })
        ));

        rh.apply_quick_probe(&Err("boom".to_string()), now);
        assert!(!rh.is_quick_probing(), "a failed check is no longer in flight");
        assert!(matches!(rh.quick_probe(), Some(QuickProbeState::Failed { error, .. }) if error == "boom"));
        assert!(rh.start_quick_probe(now), "a finished check can be redone");
        assert!(
            matches!(rh.quick_probe(), Some(QuickProbeState::Checking { last: None, .. })),
            "a failed check leaves nothing to carry forward"
        );

        rh.apply_quick_probe(&Ok(outcome(&["v1"])), now);
        assert!(!rh.is_quick_probing(), "a successful check is no longer in flight");
        assert!(
            matches!(rh.quick_probe(), Some(QuickProbeState::Checked(check)) if check.api_version.as_deref() == Some("v1"))
        );
    }

    #[test]
    fn a_re_check_keeps_what_the_last_one_measured() {
        let now = SystemTime::now();
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.apply_quick_probe(&Ok(outcome(&["v1"])), now);

        assert!(rh.start_quick_probe(now));
        let Some(QuickProbeState::Checking { last: Some(last), .. }) = rh.quick_probe() else {
            panic!("a re-check must carry the previous result");
        };
        assert_eq!(last.status_rtt, Duration::from_millis(120));
        assert_eq!(last.checked_at, now);
        assert_eq!(last.load.slots.available, 9);
    }

    #[test]
    fn quick_probe_versions_latch_and_unlatch_the_route() {
        let now = SystemTime::now();
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.set_routable(true);
        rh.apply_quick_probe(&Ok(outcome(&["v99"])), now);
        assert!(rh.is_unrecoverable());
        rh.apply_quick_probe(&Ok(outcome(&["v1"])), now);
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);

        let mut failing = RouteHealth::new(&destination(1), false, false);
        failing.set_routable(true);
        failing.apply_quick_probe(&Err("boom".to_string()), now);
        assert!(failing.is_routable(), "a failed check says nothing about the API");
    }

    // The tag flattens into the check only because it is a newtype variant over a struct.
    #[test]
    fn a_checked_quick_probe_round_trips_internally_tagged() {
        let mut rh = RouteHealth::new(&destination(1), false, false);
        rh.apply_quick_probe(&Ok(outcome(&["v1"])), SystemTime::now());

        let json = serde_json::to_string(rh.quick_probe().expect("a check was applied")).expect("serialize");
        assert!(json.contains(r#""state":"Checked""#), "{json}");

        let back: QuickProbeState = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, QuickProbeState::Checked(check) if check.api_version.as_deref() == Some("v1")));
    }
}
