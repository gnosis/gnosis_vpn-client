//! Per-destination routability from Core's graph walks; exit health lives in [`crate::probe`].
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use crate::connection::destination::{Destination, ExitKey, HopRouting};
use crate::log_output;
use crate::probe::{Health, QuickProbeOutcome, Versions, select_api_version};
use crate::serde_utils;

/// Terminal failure modes. `NotAllowed` needs a config change; `IncompatibleApiVersion` an exit upgrade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UnrecoverableReason {
    /// 0-hop without `--allow-insecure`, or 2+ hops without `--allow-experimental`.
    NotAllowed,
    /// The exit server only offers API versions we do not support.
    IncompatibleApiVersion { server_versions: Vec<String> },
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

/// The last `quickprobe` of this exit; also the wire format shown by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum QuickProbeState {
    Checking {
        #[serde(with = "serde_utils::system_time")]
        since: SystemTime,
    },
    Checked {
        #[serde(with = "serde_utils::system_time")]
        checked_at: SystemTime,
        versions: Versions,
        /// The API version this client selected from `versions`; None means incompatible.
        api_version: Option<String>,
        load: Health,
        #[serde(with = "serde_utils::duration_ms")]
        rtt: Duration,
    },
    Failed {
        #[serde(with = "serde_utils::system_time")]
        checked_at: SystemTime,
        error: String,
    },
}

pub(crate) struct RouteHealth {
    key: ExitKey,
    state: RouteHealthState,
    last_error: Option<String>,
    quick_probe: Option<QuickProbeState>,
}

impl RouteHealth {
    pub(crate) fn new(dest: &Destination, allow_insecure: bool, allow_experimental: bool) -> Self {
        Self {
            key: dest.key(),
            state: derive_initial_state(&dest.routing, allow_insecure, allow_experimental),
            last_error: None,
            quick_probe: None,
        }
    }

    pub(crate) fn quick_probe(&self) -> Option<&QuickProbeState> {
        self.quick_probe.as_ref()
    }

    /// Claims the exit for one quick probe; false while another is still running against it.
    pub(crate) fn start_quick_probe(&mut self, now: SystemTime) -> bool {
        if matches!(self.quick_probe, Some(QuickProbeState::Checking { .. })) {
            return false;
        }
        self.quick_probe = Some(QuickProbeState::Checking { since: now });
        true
    }

    /// Records what the quick probe found; a compatible API also unlatches the route like any probe does.
    pub(crate) fn apply_quick_probe(&mut self, outcome: &Result<QuickProbeOutcome, String>, now: SystemTime) {
        self.quick_probe = Some(match outcome {
            Ok(found) => {
                self.apply_api_versions(&found.versions.versions);
                QuickProbeState::Checked {
                    checked_at: now,
                    versions: found.versions.clone(),
                    api_version: found.api_version.clone(),
                    load: found.health.clone(),
                    rtt: found.rtt,
                }
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

    pub(crate) fn is_routable(&self) -> bool {
        matches!(self.state, RouteHealthState::Routable)
    }

    pub fn is_unrecoverable(&self) -> bool {
        matches!(self.state, RouteHealthState::Unrecoverable { .. })
    }

    /// Apply a graph walk result. Returns true iff the route just became routable.
    pub(crate) fn set_routable(&mut self, routable: bool) -> bool {
        let next = if routable {
            RouteHealthState::Routable
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
        let incompatible = matches!(
            self.state,
            RouteHealthState::Unrecoverable {
                reason: UnrecoverableReason::IncompatibleApiVersion { .. }
            }
        );
        if incompatible {
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

fn derive_initial_state(routing: &HopRouting, allow_insecure: bool, allow_experimental: bool) -> RouteHealthState {
    let hops = routing.hop_count();
    let insecure_without_optin = hops == 0 && !allow_insecure;
    let experimental_without_optin = hops > 1 && !allow_experimental;
    if insecure_without_optin || experimental_without_optin {
        RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::NotAllowed,
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
        }
    }
}

impl Display for QuickProbeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            QuickProbeState::Checking { since } => write!(f, "checking (since {})", log_output::elapsed(since)),
            QuickProbeState::Checked {
                checked_at,
                versions,
                api_version,
                load,
                rtt,
            } => {
                write!(
                    f,
                    "checked {} ago - RTT {:.2} s, {load}",
                    log_output::elapsed(checked_at),
                    rtt.as_secs_f32()
                )?;
                match api_version {
                    Some(api) => write!(f, ", API {api} ({versions})"),
                    None => write!(f, ", no compatible API ({versions})"),
                }
            }
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

    fn destination(hops: usize) -> Destination {
        Destination::new(
            "test".to_string(),
            Address::from([1u8; 20]),
            HopRouting::try_from(hops).unwrap(),
            Default::default(),
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
            rtt: Duration::from_millis(120),
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
    fn quick_probe_is_claimed_once_until_it_reports() {
        let now = SystemTime::now();
        let mut rh = RouteHealth::new(&destination(1), false, false);
        assert!(rh.quick_probe().is_none());
        assert!(rh.start_quick_probe(now));
        assert!(!rh.start_quick_probe(now), "still checking");
        assert!(matches!(rh.quick_probe(), Some(QuickProbeState::Checking { .. })));

        rh.apply_quick_probe(&Err("boom".to_string()), now);
        assert!(matches!(rh.quick_probe(), Some(QuickProbeState::Failed { error, .. }) if error == "boom"));
        assert!(rh.start_quick_probe(now), "a finished check can be redone");

        rh.apply_quick_probe(&Ok(outcome(&["v1"])), now);
        assert!(
            matches!(rh.quick_probe(), Some(QuickProbeState::Checked { api_version: Some(api), .. }) if api == "v1")
        );
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
}
