//! Per-destination routability from Core's graph walks; exit health lives in [`crate::probe`].
use std::time::SystemTime;

pub use gnosis_vpn_ipc::{
    ApiVersion, QuickProbeCheck, QuickProbeState, RouteHealthState, RouteHealthView, RouteWalk, UnrecoverableReason,
};

use crate::connection::destination::{Destination, ExitKey, HopRouting};
use crate::probe::{QuickProbeOutcome, select_api_version};

pub(crate) struct RouteHealth {
    key: ExitKey,
    state: RouteHealthState,
    last_error: Option<String>,
    last_walk: Option<RouteWalk>,
    quick_probe: Option<QuickProbeState>,
}

impl RouteHealth {
    pub(crate) fn new(dest: &Destination, allow_insecure: bool, allow_experimental: bool) -> Self {
        Self {
            key: dest.key(),
            state: derive_initial_state(&dest.routing, allow_insecure, allow_experimental),
            last_error: None,
            last_walk: None,
            quick_probe: None,
        }
    }

    pub(crate) fn view(&self) -> RouteHealthView {
        RouteHealthView {
            state: self.state.clone(),
            last_error: self.last_error().map(str::to_owned),
            walk: self.walk().cloned(),
            quick_probe: self.quick_probe().cloned(),
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
                self.apply_api_versions(&found.versions.api);
                QuickProbeState::Checked(QuickProbeCheck {
                    checked_at: now,
                    versions: found.versions.clone(),
                    api_version: found.api_version,
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

    pub(crate) fn is_routable(&self) -> bool {
        matches!(self.state, RouteHealthState::Routable)
    }

    pub fn is_unrecoverable(&self) -> bool {
        matches!(self.state, RouteHealthState::Unrecoverable { .. })
    }

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
    pub(crate) fn apply_api_versions(&mut self, offered: &[ApiVersion]) {
        match select_api_version(offered) {
            Some(_) => self.clear_incompatible(),
            None => self.set_incompatible(offered.to_vec()),
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
    fn set_incompatible(&mut self, offered: Vec<ApiVersion>) {
        if self.is_unrecoverable() {
            return;
        }
        tracing::warn!(destination = %self.key, ?offered, "exit offers no compatible API version");
        self.state = RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::IncompatibleApiVersion { offered },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Address, DestinationSource};
    use std::time::Duration;

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

    fn outcome(offered: &[&str]) -> QuickProbeOutcome {
        let versions = crate::probe::ExitVersions {
            api: offered.iter().map(|v| v.parse().expect("api label")).collect(),
            server: None,
        };
        QuickProbeOutcome {
            api_version: select_api_version(&versions.api),
            versions,
            health: crate::probe::Health {
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
        }
    }

    fn walk_with_paths(now: SystemTime) -> RouteWalk {
        RouteWalk::Paths {
            walked_at: now,
            count: 3,
            distinct_first_relays: 2,
            best_relays: vec![gnosis_vpn_ipc::Address::new([2u8; 20])],
            best_value: 0.75,
            return_count: 2,
            return_best_relays: vec![gnosis_vpn_ipc::Address::new([3u8; 20])],
            return_best_value: 0.5,
        }
    }

    fn no_path(forward_count: u32, return_count: u32) -> RouteWalk {
        RouteWalk::NoPath {
            walked_at: SystemTime::now(),
            forward_count,
            return_count,
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

        assert!(!rh.apply_walk(no_path(0, 0)));
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);

        assert!(!rh.apply_walk(RouteWalk::NotAnnounced { walked_at: now }));
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);
        assert!(matches!(rh.walk(), Some(RouteWalk::NotAnnounced { .. })));
    }

    #[test]
    fn a_missing_return_path_keeps_the_route_not_routable() {
        let mut rh = RouteHealth::new(&destination(1), false, false);
        assert!(rh.apply_walk(walk_with_paths(SystemTime::now())));
        assert!(!rh.apply_walk(no_path(3, 0)));
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);
        assert!(
            rh.walk()
                .is_some_and(|walk| walk.to_string().contains("(forward 3, return 0)"))
        );
    }

    #[test]
    fn unresolved_relays_are_not_reported_as_direct() {
        let unresolved = RouteWalk::Paths {
            walked_at: SystemTime::now(),
            count: 1,
            distinct_first_relays: 1,
            best_relays: Vec::new(),
            best_value: 0.5,
            return_count: 1,
            return_best_relays: Vec::new(),
            return_best_value: 0.5,
        }
        .to_string();
        assert!(!unresolved.contains("direct"), "{unresolved}");
        assert!(unresolved.contains("return 1, unresolved relays"), "{unresolved}");

        let zero_hop = RouteWalk::Paths {
            walked_at: SystemTime::now(),
            count: 1,
            distinct_first_relays: 0,
            best_relays: Vec::new(),
            best_value: 1.0,
            return_count: 1,
            return_best_relays: Vec::new(),
            return_best_value: 1.0,
        }
        .to_string();
        assert!(zero_hop.contains("1 path, direct"), "{zero_hop}");
        assert!(zero_hop.contains("return 1, direct"), "{zero_hop}");
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
        rh.apply_api_versions(&[ApiVersion::new(99)]);
        assert!(rh.is_unrecoverable());
        rh.apply_api_versions(&[ApiVersion::new(1)]);
        assert_eq!(*rh.state(), RouteHealthState::NotRoutable);

        let mut latched = RouteHealth::new(&destination(0), false, false);
        latched.apply_api_versions(&[ApiVersion::new(99)]);
        latched.apply_api_versions(&[ApiVersion::new(1)]);
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
        incompatible.apply_api_versions(&[ApiVersion::new(99)]);
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
            matches!(rh.quick_probe(), Some(QuickProbeState::Checked(check)) if check.api_version == Some(ApiVersion::new(1)))
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
        assert!(json.contains(r#""type":"checked""#), "{json}");

        let back: QuickProbeState = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, QuickProbeState::Checked(check) if check.api_version == Some(ApiVersion::new(1))));
    }
}
