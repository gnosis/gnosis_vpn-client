//! Network probing policy: a level staircase driven by route readiness, plus the exits probing focuses on.
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use gnosis_vpn_ipc::{ProbingLevel, ProbingReason};

use crate::connection::destination::ExitKey;
use crate::route_health::{RouteHealth, RouteWalk};

/// The app's weak-route banner threshold, so "ready" and "no banner" agree.
pub(crate) const WEAK_ROUTE_VALUE: f64 = 0.9;
/// Twice the slowest walk interval: an older walk says nothing about the route now.
const WALK_FRESHNESS: Duration = Duration::from_secs(120);
const MAX_FOCUS: usize = 10;
const MAX_PINNED: usize = 5;
/// The length of the app's former recent list.
const MAX_HISTORY: usize = 6;
/// How long readiness, or its absence, must last for one more level step.
const STEP_HOLD: Duration = Duration::from_secs(30);
const ROTATION_PERIOD: Duration = Duration::from_secs(5 * 60);
const HISTORY_FILE: &str = "recent_exits.json";
/// With probing off only session traffic moves the scores, so walks can idle.
const WALK_WITHOUT_PROBING: Duration = Duration::from_secs(60);

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
}

struct Readiness {
    ready: bool,
    since: Instant,
}

pub(crate) struct NetworkProbing {
    /// The staircase level while idle; connecting and connected override it without moving it.
    level: ProbingLevel,
    /// None until the first idle decision, and again after every connection.
    readiness: Option<Readiness>,
    rotation: Rotation,
    /// Recently connected exits, newest first.
    history: Vec<ExitKey>,
}

impl NetworkProbing {
    pub(crate) fn new(history: Vec<ExitKey>) -> Self {
        Self {
            level: ProbingLevel::High,
            readiness: None,
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

    /// One level up; readiness walks it back down after the usual hold.
    pub(crate) fn boost(&mut self, now: Instant) {
        self.level = faster(self.level);
        if let Some(readiness) = &mut self.readiness {
            readiness.since = now;
        }
    }

    pub(crate) fn decide(&mut self, inputs: &Inputs, now: Instant) -> Plan {
        let focus = self.focus(inputs, now);
        let (level, reason) = match inputs.activity {
            Activity::Connecting(_) => (ProbingLevel::High, ProbingReason::Connecting),
            Activity::Connected(_) => (ProbingLevel::Low, ProbingReason::Connected),
            Activity::Idle => self.step(inputs.any_ready, now),
        };
        if inputs.activity != Activity::Idle {
            self.readiness = None;
        }
        Plan { level, focus, reason }
    }

    /// The graph already smooths over 60 s, so readiness moves the level at once and again per hold.
    fn step(&mut self, ready: bool, now: Instant) -> (ProbingLevel, ProbingReason) {
        let due = match &mut self.readiness {
            Some(readiness) if readiness.ready == ready => {
                let held = now.saturating_duration_since(readiness.since) >= STEP_HOLD;
                if held {
                    readiness.since = now;
                }
                held
            }
            _ => {
                self.readiness = Some(Readiness { ready, since: now });
                true
            }
        };
        if due {
            self.level = if ready { slower(self.level) } else { faster(self.level) };
        }
        let held_for = self
            .readiness
            .as_ref()
            .map_or(Duration::ZERO, |r| now.saturating_duration_since(r.since));
        let reason = if ready {
            ProbingReason::Ready { held_for }
        } else {
            ProbingReason::NotReady { held_for }
        };
        (self.level, reason)
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

/// The policy never holds Off; that only comes from configuration.
fn slower(level: ProbingLevel) -> ProbingLevel {
    match level {
        ProbingLevel::High => ProbingLevel::Medium,
        ProbingLevel::Medium | ProbingLevel::Low | ProbingLevel::Off => ProbingLevel::Low,
    }
}

fn faster(level: ProbingLevel) -> ProbingLevel {
    match level {
        ProbingLevel::Low | ProbingLevel::Off => ProbingLevel::Medium,
        ProbingLevel::Medium | ProbingLevel::High => ProbingLevel::High,
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

/// Time between edgli's probing rounds: its 3 s base (edge-client `src/client.rs:332`) scaled by the level.
fn probing_round(level: ProbingLevel) -> Option<Duration> {
    match level {
        ProbingLevel::Off => None,
        ProbingLevel::Low => Some(Duration::from_secs(30)),
        ProbingLevel::Medium => Some(Duration::from_secs(6)),
        ProbingLevel::High => Some(Duration::from_secs(3)),
    }
}

/// 1.5 rounds: even with the walk's ±25 % jitter, each walk sees a new round and skips at most one.
pub(crate) fn walk_interval(level: ProbingLevel) -> Duration {
    probing_round(level).map_or(WALK_WITHOUT_PROBING, |round| round * 3 / 2)
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

    fn idle(any_ready: bool) -> Inputs {
        Inputs {
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
    fn a_network_that_never_gets_ready_stays_high() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (plan, _) = run(&mut probing, &idle(false), Instant::now(), secs(600));
        assert_eq!(plan.level, ProbingLevel::High);
        assert!(matches!(plan.reason, ProbingReason::NotReady { .. }));
    }

    #[test]
    fn readiness_steps_down_at_once_and_again_after_each_hold() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        assert_eq!(probing.decide(&idle(true), start).level, ProbingLevel::Medium);
        assert_eq!(
            probing.decide(&idle(true), start + secs(25)).level,
            ProbingLevel::Medium
        );
        let plan = probing.decide(&idle(true), start + STEP_HOLD);
        assert_eq!(
            (plan.level, plan.reason),
            (ProbingLevel::Low, ProbingReason::Ready { held_for: secs(0) })
        );
        let (plan, _) = run(&mut probing, &idle(true), start + STEP_HOLD, secs(120));
        assert_eq!(plan.level, ProbingLevel::Low);
    }

    #[test]
    fn losing_readiness_steps_up_at_once_and_again_after_each_hold() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (_, at) = run(&mut probing, &idle(true), Instant::now(), STEP_HOLD);
        assert_eq!(probing.decide(&idle(false), at + secs(5)).level, ProbingLevel::Medium);
        let plan = probing.decide(&idle(false), at + secs(5) + STEP_HOLD);
        assert_eq!(plan.level, ProbingLevel::High);
    }

    #[test]
    fn a_boost_steps_up_and_restarts_the_hold() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (_, at) = run(&mut probing, &idle(true), Instant::now(), STEP_HOLD);
        probing.boost(at + secs(1));
        assert_eq!(probing.decide(&idle(true), at + secs(5)).level, ProbingLevel::Medium);
        assert_eq!(probing.decide(&idle(true), at + STEP_HOLD).level, ProbingLevel::Medium);
        assert_eq!(
            probing.decide(&idle(true), at + secs(1) + STEP_HOLD).level,
            ProbingLevel::Low
        );
    }

    #[test]
    fn connecting_is_high_and_focuses_on_the_target_only() {
        let mut probing = NetworkProbing::new(vec![key(1)]);
        let inputs = Inputs {
            activity: Activity::Connecting(key(2)),
            ..idle(false)
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
            ..idle(true)
        };
        let plan = probing.decide(&inputs, Instant::now());
        assert_eq!(plan.level, ProbingLevel::Low);
        assert_eq!(plan.reason, ProbingReason::Connected);
        assert!(!plan.focus.contains(&key(1)));
        assert_eq!(plan.focus[0], key(2));
    }

    #[test]
    fn after_a_connection_the_staircase_resumes_where_it_was() {
        let mut probing = NetworkProbing::new(Vec::new());
        let (_, at) = run(&mut probing, &idle(true), Instant::now(), STEP_HOLD);
        let connected = Inputs {
            activity: Activity::Connected(key(1)),
            ..idle(false)
        };
        let (_, at) = run(&mut probing, &connected, at + secs(5), secs(300));
        assert_eq!(probing.decide(&idle(true), at + secs(5)).level, ProbingLevel::Low);
        assert_eq!(probing.decide(&idle(false), at + secs(10)).level, ProbingLevel::Medium);
    }

    #[test]
    fn focus_pins_recent_exits_first_and_caps_at_10() {
        let history = keys(1..=6);
        let mut probing = NetworkProbing::new(history);
        let inputs = Inputs {
            selectable: keys(1..=20),
            ..idle(false)
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
        let plan = probing.decide(&idle(false), Instant::now());
        assert!(!plan.focus.contains(&key(9)));
        let start = Instant::now();
        probing.decide(&idle(false), start);
        let fewer = Inputs {
            selectable: vec![key(1)],
            ..idle(false)
        };
        assert_eq!(probing.decide(&fewer, start + secs(5)).focus, vec![key(1)]);
    }

    #[test]
    fn new_exits_enter_the_rotation_at_the_front() {
        let mut probing = NetworkProbing::new(Vec::new());
        let start = Instant::now();
        let many = Inputs {
            selectable: keys(1..=30),
            ..idle(false)
        };
        probing.decide(&many, start);
        let one_more = Inputs {
            selectable: keys(1..=31),
            ..idle(false)
        };
        assert!(probing.decide(&one_more, start + secs(5)).focus.contains(&key(31)));
    }

    #[test]
    fn rotation_advances_every_5_min_and_wraps() {
        let mut probing = NetworkProbing::new(Vec::new());
        let inputs = Inputs {
            selectable: keys(1..=25),
            ..idle(false)
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
    fn jittered_walks_span_more_than_one_round_and_less_than_two() {
        for level in [ProbingLevel::Low, ProbingLevel::Medium, ProbingLevel::High] {
            let round = probing_round(level).expect("probing");
            let interval = walk_interval(level);
            assert!(interval.mul_f64(0.75) > round, "{level:?} may walk twice in one round");
            assert!(interval.mul_f64(1.25) < round * 2, "{level:?} may skip two rounds");
        }
        assert_eq!(walk_interval(ProbingLevel::Off), WALK_WITHOUT_PROBING);
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
