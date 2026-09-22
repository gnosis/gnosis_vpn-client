//! Per-destination routability from Core's graph walks; exit health lives in [`crate::probe`].
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};

use crate::connection::destination::{Destination, ExitKey, HopRouting};
use crate::probe::select_api_version;

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

pub(crate) struct RouteHealth {
    key: ExitKey,
    state: RouteHealthState,
    last_error: Option<String>,
}

impl RouteHealth {
    pub(crate) fn new(dest: &Destination, allow_insecure: bool, allow_experimental: bool) -> Self {
        Self {
            key: dest.key(),
            state: derive_initial_state(&dest.routing, allow_insecure, allow_experimental),
            last_error: None,
        }
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
}
