//! Network probing policy: the edge client's probing level and focus, decided from phase, route walks and boosts.
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use gnosis_vpn_ipc::{ProbingLevel, ProbingReason};

use crate::connection::destination::ExitKey;
use crate::route_health::{RouteHealth, RouteWalk};

/// The app's weak-route banner threshold, so "warm" and "no banner" agree.
pub(crate) const WEAK_ROUTE_VALUE: f64 = 0.9;
/// Twice the lazy walk interval: an older walk says nothing about the route now.
const WALK_FRESHNESS: Duration = Duration::from_secs(120);
const MAX_FOCUS: usize = 10;
const MAX_PINNED: usize = 5;
/// The length of the app's former recent list.
const MAX_HISTORY: usize = 6;
const READY_HOLD: Duration = Duration::from_secs(30);
const WARM_UP_CAP: Duration = Duration::from_secs(5 * 60);
const NOT_READY_HOLD: Duration = Duration::from_secs(60);
/// Without it a network where nothing gets ready cycles 5 min High / 60 s Medium forever.
const CAP_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const ROTATION_PERIOD: Duration = Duration::from_secs(5 * 60);
const HISTORY_FILE: &str = "recent_exits.json";

/// Operator override of the adaptive policy, e.g. for measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbingPolicy {
    #[default]
    Adaptive,
    Fixed {
        level: ProbingLevel,
    },
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    Idle,
    Connecting(ExitKey),
    Connected(ExitKey),
}

pub(crate) struct Inputs {
    /// A client asked for Medium instead of Low once warm.
    pub boosted: bool,
    pub activity: Activity,
    /// Exits worth probing; order does not matter, the rotation shuffles them.
    pub selectable: Vec<ExitKey>,
    /// Some selectable exit is ready, see [`is_ready`].
    pub any_ready: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub level: ProbingLevel,
    pub focus: Vec<ExitKey>,
    pub reason: ProbingReason,
}

impl Plan {
    /// What configuration dictates instead of the adaptive policy; `None` for adaptive.
    pub(crate) fn from_override(policy: ProbingPolicy) -> Option<Plan> {
        let level = match policy {
            ProbingPolicy::Adaptive => return None,
            ProbingPolicy::Fixed { level } => level,
            ProbingPolicy::Off => ProbingLevel::Off,
        };
        Some(Plan {
            level,
            focus: Vec::new(),
            reason: ProbingReason::Fixed,
        })
    }

    /// Level and focus are all the edge client sees; a changed reason alone needs no re-apply.
    pub(crate) fn same_effect(&self, other: &Plan) -> bool {
        self.level == other.level && self.focus == other.focus
    }
}

enum Warmth {
    Cold {
        high_for: Duration,
    },
    /// `capped_at` is set when the warm-up cap, not readiness, ended the warm-up.
    Warm {
        capped_at: Option<Instant>,
    },
}

pub(crate) struct NetworkProbing {
    warmth: Warmth,
    ready_since: Option<Instant>,
    not_ready_since: Option<Instant>,
    last_decided_at: Option<Instant>,
    rotation: Rotation,
    /// Recently connected exits, newest first.
    history: Vec<ExitKey>,
}

impl NetworkProbing {
    pub(crate) fn new(history: Vec<ExitKey>) -> Self {
        Self {
            warmth: Warmth::Cold {
                high_for: Duration::ZERO,
            },
            ready_since: None,
            not_ready_since: None,
            last_decided_at: None,
            rotation: Rotation::default(),
            history,
        }
    }

    pub(crate) fn history(&self) -> &[ExitKey] {
        &self.history
    }

    /// Returns the updated history for the caller to persist.
    pub(crate) fn remember_connected(&mut self, key: ExitKey) -> &[ExitKey] {
        self.history.retain(|k| *k != key);
        self.history.insert(0, key);
        self.history.truncate(MAX_HISTORY);
        &self.history
    }

    pub(crate) fn decide(&mut self, inputs: &Inputs, now: Instant) -> Plan {
        self.accrue_high(now);
        self.track_readiness(inputs, now);
        self.update_warmth(inputs, now);
        let (level, reason) = self.level(inputs);
        let focus = self.focus(inputs, now);
        Plan { level, focus, reason }
    }

    fn accrue_high(&mut self, now: Instant) {
        let elapsed = self
            .last_decided_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        self.last_decided_at = Some(now);
        // Cold always means High, so cold time is warm-up time.
        if let Warmth::Cold { high_for } = &mut self.warmth {
            *high_for += elapsed;
        }
    }

    /// Readiness only counts while idle, so a disconnect gets the full not-ready grace before re-raising.
    fn track_readiness(&mut self, inputs: &Inputs, now: Instant) {
        if inputs.activity != Activity::Idle {
            self.ready_since = None;
            self.not_ready_since = None;
            return;
        }
        if inputs.any_ready {
            self.not_ready_since = None;
            self.ready_since.get_or_insert(now);
        } else {
            self.ready_since = None;
            self.not_ready_since.get_or_insert(now);
        }
    }

    fn update_warmth(&mut self, inputs: &Inputs, now: Instant) {
        if inputs.activity != Activity::Idle {
            self.warmth = Warmth::Warm { capped_at: None };
            return;
        }
        let held_for =
            |since: Option<Instant>, hold: Duration| since.is_some_and(|at| now.saturating_duration_since(at) >= hold);
        match self.warmth {
            Warmth::Cold { high_for } => {
                if held_for(self.ready_since, READY_HOLD) {
                    self.warmth = Warmth::Warm { capped_at: None };
                } else if high_for >= WARM_UP_CAP {
                    self.warmth = Warmth::Warm { capped_at: Some(now) };
                }
            }
            Warmth::Warm { capped_at } => {
                let cooling_down = capped_at.is_some_and(|at| now.saturating_duration_since(at) < CAP_COOLDOWN);
                if held_for(self.not_ready_since, NOT_READY_HOLD) && !cooling_down {
                    self.warmth = Warmth::Cold {
                        high_for: Duration::ZERO,
                    };
                }
            }
        }
    }

    fn level(&self, inputs: &Inputs) -> (ProbingLevel, ProbingReason) {
        match inputs.activity {
            Activity::Connecting(_) => return (ProbingLevel::High, ProbingReason::Connecting),
            Activity::Connected(_) => return (ProbingLevel::Low, ProbingReason::Connected),
            Activity::Idle => {}
        }
        match self.warmth {
            Warmth::Cold { high_for } => (ProbingLevel::High, ProbingReason::WarmingUp { high_for }),
            Warmth::Warm { .. } if inputs.boosted => (ProbingLevel::Medium, ProbingReason::Warm),
            Warmth::Warm { .. } => (ProbingLevel::Low, ProbingReason::Idle),
        }
    }

    /// Recent exits first, the rest of the slots rotate through every other selectable exit.
    fn focus(&mut self, inputs: &Inputs, now: Instant) -> Vec<ExitKey> {
        let connected = match inputs.activity {
            Activity::Connecting(target) => return vec![target],
            Activity::Connected(key) => Some(key),
            Activity::Idle => None,
        };
        let eligible = |key: &ExitKey| inputs.selectable.contains(key) && Some(*key) != connected;
        let mut focus: Vec<ExitKey> = self
            .history
            .iter()
            .filter(|key| eligible(key))
            .take(MAX_PINNED)
            .copied()
            .collect();
        let free = MAX_FOCUS - focus.len();
        self.rotation.sync(&inputs.selectable);
        self.rotation.advance(free, now);
        let rotating = self.rotation.window(free, |key| eligible(key) && !focus.contains(key));
        focus.extend(rotating);
        focus
    }
}

/// A shuffled ring of exits; the focus window slides along it so every exit gets probed over time.
#[derive(Default)]
struct Rotation {
    queue: VecDeque<ExitKey>,
    advanced_at: Option<Instant>,
}

impl Rotation {
    /// Gone exits leave; new ones go to the front so they are probed soon.
    fn sync(&mut self, selectable: &[ExitKey]) {
        self.queue.retain(|key| selectable.contains(key));
        let mut fresh: Vec<ExitKey> = selectable
            .iter()
            .filter(|key| !self.queue.contains(key))
            .copied()
            .collect();
        fresh.shuffle(&mut rand::rng());
        for key in fresh {
            self.queue.push_front(key);
        }
    }

    fn advance(&mut self, step: usize, now: Instant) {
        let Some(advanced_at) = self.advanced_at else {
            self.advanced_at = Some(now);
            return;
        };
        if now.saturating_duration_since(advanced_at) < ROTATION_PERIOD {
            return;
        }
        self.advanced_at = Some(now);
        if !self.queue.is_empty() {
            let len = self.queue.len();
            self.queue.rotate_left(step % len);
        }
    }

    fn window(&self, count: usize, keep: impl Fn(&ExitKey) -> bool) -> Vec<ExitKey> {
        self.queue.iter().filter(|key| keep(key)).take(count).copied().collect()
    }
}

/// Unrecoverable and unannounced exits cannot be reached, so probing towards them is wasted.
pub(crate) fn is_selectable(health: &RouteHealth) -> bool {
    let unannounced = matches!(health.walk(), Some(RouteWalk::NotAnnounced { .. }));
    !health.is_unrecoverable() && !unannounced
}

pub(crate) fn is_ready(health: &RouteHealth, now: SystemTime) -> bool {
    health.is_routable() && health.walk().is_some_and(|walk| walk_is_ready(walk, now))
}

fn walk_is_ready(walk: &RouteWalk, now: SystemTime) -> bool {
    let RouteWalk::Paths {
        walked_at, best_value, ..
    } = walk
    else {
        return false;
    };
    // A walk stamped in the future (clock step) counts as fresh.
    let age = now.duration_since(*walked_at).unwrap_or(Duration::ZERO);
    age <= WALK_FRESHNESS && *best_value >= WEAK_ROUTE_VALUE
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    recent: Vec<String>,
}

/// The history is a hint: anything unreadable starts empty instead of being migrated.
pub(crate) async fn load_history(dir: &Path) -> Vec<ExitKey> {
    let path = dir.join(HISTORY_FILE);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read recent exits - starting empty");
            return Vec::new();
        }
    };
    match parse_history(&content) {
        Ok(history) => history,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot parse recent exits - starting empty");
            Vec::new()
        }
    }
}

fn parse_history(content: &str) -> Result<Vec<ExitKey>, String> {
    let file: HistoryFile = serde_json::from_str(content).map_err(|e| e.to_string())?;
    file.recent.iter().take(MAX_HISTORY).map(|key| key.parse()).collect()
}

/// Temp file plus rename, so a crash never leaves half a file behind.
pub(crate) async fn save_history(dir: &Path, history: &[ExitKey]) -> std::io::Result<()> {
    let file = HistoryFile {
        recent: history.iter().map(ToString::to_string).collect(),
    };
    let json = serde_json::to_string(&file).map_err(std::io::Error::other)?;
    let path = dir.join(HISTORY_FILE);
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, json).await?;
    tokio::fs::rename(&tmp, &path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Address, default_path};

    fn key(byte: u8) -> ExitKey {
        ExitKey {
            address: Address::from([byte; 20]),
            routing: default_path(),
        }
    }

    fn keys(bytes: std::ops::RangeInclusive<u8>) -> Vec<ExitKey> {
        bytes.map(key).collect()
    }

    fn idle(boosted: bool, any_ready: bool) -> Inputs {
        Inputs {
            boosted,
            activity: Activity::Idle,
            selectable: keys(1..=3),
            any_ready,
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Decides every 5 s, as the worker's tick does, until `until` has passed.
    fn run(probing: &mut NetworkProbing, inputs: &Inputs, start: Instant, until: Duration) -> (Plan, Instant) {
        let mut at = start;
        let mut plan = probing.decide(inputs, at);
        while at < start + until {
            at += secs(5);
            plan = probing.decide(inputs, at);
        }
        (plan, at)
    }

    #[test]
    fn a_fresh_worker_starts_cold_at_high_without_a_boost() {
        let mut probing = NetworkProbing::new(Vec::new());
        let plan = probing.decide(&idle(false, false), Instant::now());
        assert_eq!(plan.level, ProbingLevel::High);
        assert!(matches!(plan.reason, ProbingReason::WarmingUp { .. }));
    }

    #[test]
    fn warm_is_low_without_a_boost_and_medium_with_one() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (plan, at) = run(&mut probing, &idle(false, true), Instant::now(), READY_HOLD);
        assert_eq!((plan.level, plan.reason), (ProbingLevel::Low, ProbingReason::Idle));
        let plan = probing.decide(&idle(true, true), at + secs(5));
        assert_eq!((plan.level, plan.reason), (ProbingLevel::Medium, ProbingReason::Warm));
    }

    #[test]
    fn connecting_is_high_and_focuses_on_the_target_only() {
        let mut probing = NetworkProbing::new(vec![key(1)]);
        let inputs = Inputs {
            activity: Activity::Connecting(key(2)),
            ..idle(false, false)
        };
        let plan = probing.decide(&inputs, Instant::now());
        assert_eq!(plan.level, ProbingLevel::High);
        assert_eq!(plan.reason, ProbingReason::Connecting);
        assert_eq!(plan.focus, vec![key(2)]);
    }

    #[test]
    fn connected_is_low_and_leaves_the_connected_exit_out_of_focus() {
        let mut probing = NetworkProbing::new(vec![key(1), key(2)]);
        let inputs = Inputs {
            activity: Activity::Connected(key(1)),
            ..idle(true, true)
        };
        let plan = probing.decide(&inputs, Instant::now());
        assert_eq!(plan.level, ProbingLevel::Low);
        assert_eq!(plan.reason, ProbingReason::Connected);
        assert!(!plan.focus.contains(&key(1)));
        assert_eq!(plan.focus[0], key(2));
    }

    #[test]
    fn warms_to_medium_after_readiness_holds_30_s() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        let (plan, _) = run(&mut probing, &idle(true, true), start, secs(25));
        assert_eq!(plan.level, ProbingLevel::High);
        let (plan, _) = run(&mut probing, &idle(true, true), start + secs(25), secs(5));
        assert_eq!(plan.level, ProbingLevel::Medium);
        assert_eq!(plan.reason, ProbingReason::Warm);
    }

    #[test]
    fn warms_to_medium_after_5_min_of_high_without_readiness() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (plan, _) = run(&mut probing, &idle(true, false), Instant::now(), WARM_UP_CAP);
        assert_eq!(plan.level, ProbingLevel::Medium);
    }

    #[test]
    fn re_raises_to_high_after_60_s_not_ready() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (_, at) = run(&mut probing, &idle(true, true), Instant::now(), READY_HOLD);
        let (plan, at) = run(&mut probing, &idle(true, false), at + secs(5), secs(55));
        assert_eq!(plan.level, ProbingLevel::Medium);
        let plan = probing.decide(&idle(true, false), at + secs(5));
        assert_eq!(plan.level, ProbingLevel::High);
    }

    #[test]
    fn no_re_raise_within_30_min_of_a_capped_warm_up() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        let (_, at) = run(&mut probing, &idle(true, false), start, WARM_UP_CAP);
        let (plan, at) = run(&mut probing, &idle(true, false), at, CAP_COOLDOWN - secs(10));
        assert_eq!(plan.level, ProbingLevel::Medium);
        let (plan, _) = run(&mut probing, &idle(true, false), at, secs(10));
        assert_eq!(plan.level, ProbingLevel::High);
    }

    #[test]
    fn a_disconnect_lands_warm() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        let connected = Inputs {
            activity: Activity::Connected(key(1)),
            ..idle(true, false)
        };
        let (_, at) = run(&mut probing, &connected, start, secs(300));
        let plan = probing.decide(&idle(true, false), at + secs(5));
        assert_eq!(plan.level, ProbingLevel::Medium);
    }

    #[test]
    fn focus_pins_recent_exits_first_and_caps_at_10() {
        let history = keys(1..=6);
        let mut probing = NetworkProbing::new(history);
        let inputs = Inputs {
            selectable: keys(1..=20),
            ..idle(false, false)
        };
        let plan = probing.decide(&inputs, Instant::now());
        assert_eq!(plan.focus.len(), MAX_FOCUS);
        assert_eq!(plan.focus[..MAX_PINNED], keys(1..=5)[..]);
        let rotating = &plan.focus[MAX_PINNED..];
        assert!(rotating.iter().all(|k| !keys(1..=5).contains(k)), "{rotating:?}");
    }

    #[test]
    fn focus_drops_exits_that_are_no_longer_selectable() {
        let mut probing = NetworkProbing::new(vec![key(9)]);
        let plan = probing.decide(&idle(false, false), Instant::now());
        assert!(!plan.focus.contains(&key(9)));
        let start = Instant::now();
        probing.decide(&idle(false, false), start);
        let fewer = Inputs {
            selectable: vec![key(1)],
            ..idle(false, false)
        };
        assert_eq!(probing.decide(&fewer, start + secs(5)).focus, vec![key(1)]);
    }

    #[test]
    fn new_exits_enter_the_rotation_at_the_front() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        let many = Inputs {
            selectable: keys(1..=30),
            ..idle(false, false)
        };
        probing.decide(&many, start);
        let one_more = Inputs {
            selectable: keys(1..=31),
            ..idle(false, false)
        };
        assert!(probing.decide(&one_more, start + secs(5)).focus.contains(&key(31)));
    }

    #[test]
    fn rotation_advances_every_5_min_and_wraps() {
        let mut probing = NetworkProbing::new(Vec::new());
        let inputs = Inputs {
            selectable: keys(1..=25),
            ..idle(false, false)
        };
        let start = Instant::now();
        let first = probing.decide(&inputs, start).focus;
        assert_eq!(probing.decide(&inputs, start + secs(60)).focus, first);
        let second = probing.decide(&inputs, start + ROTATION_PERIOD).focus;
        assert!(second.iter().all(|k| !first.contains(k)), "{first:?} -> {second:?}");
        let mut seen: Vec<ExitKey> = first.into_iter().chain(second).collect();
        let third = probing.decide(&inputs, start + ROTATION_PERIOD * 2).focus;
        seen.extend(third.iter().copied());
        assert!(
            keys(1..=25).iter().all(|k| seen.contains(k)),
            "every exit is probed in one cycle"
        );
        assert_eq!(third.len(), MAX_FOCUS, "the window wraps around the ring");
    }

    #[test]
    fn remember_connected_dedups_and_caps_the_history() {
        let mut probing = NetworkProbing::new(keys(1..=6));
        let history = probing.remember_connected(key(3)).to_vec();
        assert_eq!(history, vec![key(3), key(1), key(2), key(4), key(5), key(6)]);
        let history = probing.remember_connected(key(7)).to_vec();
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0], key(7));
        assert!(!history.contains(&key(6)));
    }

    #[test]
    fn stale_or_weak_walks_are_not_ready() {
        let now = SystemTime::now();
        let walk = |age: Duration, best_value: f64| RouteWalk::Paths {
            walked_at: now - age,
            count: 3,
            distinct_first_relays: 2,
            best_relays: Vec::new(),
            best_value,
            return_count: 3,
            return_best_relays: Vec::new(),
            return_best_value: 1.0,
        };
        assert!(walk_is_ready(&walk(secs(10), 0.95), now));
        assert!(!walk_is_ready(&walk(secs(10), 0.5), now));
        assert!(!walk_is_ready(&walk(WALK_FRESHNESS + secs(1), 0.95), now));
        assert!(!walk_is_ready(&RouteWalk::NotAnnounced { walked_at: now }, now));
    }

    #[tokio::test]
    async fn history_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_history(dir.path(), &keys(1..=3)).await.expect("save");
        assert_eq!(load_history(dir.path()).await, keys(1..=3));
    }

    #[tokio::test]
    async fn missing_or_corrupt_history_starts_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_history(dir.path()).await.is_empty());
        tokio::fs::write(dir.path().join(HISTORY_FILE), r#"{"recent":["nope"]}"#)
            .await
            .expect("write");
        assert!(load_history(dir.path()).await.is_empty());
    }

    #[test]
    fn overrides_bypass_the_adaptive_policy() {
        assert_eq!(Plan::from_override(ProbingPolicy::Adaptive), None);
        let off = Plan::from_override(ProbingPolicy::Off).expect("override");
        assert_eq!(off.level, ProbingLevel::Off);
        let fixed = Plan::from_override(ProbingPolicy::Fixed {
            level: ProbingLevel::High,
        })
        .expect("override");
        assert_eq!((fixed.level, fixed.reason), (ProbingLevel::High, ProbingReason::Fixed));
    }
}
