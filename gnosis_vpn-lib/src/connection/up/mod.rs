use edgli::hopr_lib::exports::transport::{HoprSessionConfigurator, SurbBalancerConfig};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::net;
use std::time::{Duration, SystemTime};

use crate::connection::destination::Destination;
use crate::connection::options::SurbConfigError;
use crate::gvpn_client::Registration;
use crate::hopr::HoprError;
use crate::hopr::types::SessionClientMetadata;
use crate::wg_tunnel::{self, TunnelStatsSample};
use crate::wireguard::WireGuard;
use crate::{gvpn_client, log_output, remote_data, wireguard};

mod demand;
pub(crate) mod runner;

#[derive(Debug)]
pub enum Event {
    Progress(Box<Progress>),
    Setback(Box<Setback>),
}

#[derive(Clone, Debug)]
pub enum SessionKind {
    Ping,
    Main,
}

#[derive(Clone, Debug)]
pub enum Progress {
    ResolveBlokliIps,
    GenerateWg(Vec<net::Ipv4Addr>),
    OpenBridge(WireGuard),
    BridgeOpened(SessionClientMetadata),
    RegisterWg,
    OpenPing(Registration),
    BridgeClosed,
    PeerIps,
    KillswitchLockdown,
    StaticWgTunnel(SessionClientMetadata),
    /// Handle to adjust the active session's SURB balancer, retained on `Up`
    /// so `core` can reconfigure it from live telemetry, not just once here.
    SessionConfigurator(HoprSessionConfigurator),
    Ping,
    AdjustToMain(Duration),
    /// Sets the SURB balancer's desired target; `core` slews toward it over time.
    SetSurbTarget {
        applied: SurbBalancerConfig,
        target: SurbBalancerConfig,
    },
}

/// How long a SURB balancer target change takes to fully converge.
const SURB_RAMP_DURATION: Duration = Duration::from_secs(60);

/// Max per-second change per SURB balancer knob, so the follower converges gradually instead of jumping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurbSlewRate {
    /// Max change in `target_surb_buffer_size`, per second.
    buffer_per_sec: u64,
    /// Max change in `max_surbs_per_sec`, per second.
    rate_per_sec: u64,
}

impl SurbSlewRate {
    /// Rate that closes the gap between `applied` and `target` over `duration`.
    fn to_cover(applied: SurbBalancerConfig, target: SurbBalancerConfig, duration: Duration) -> Self {
        let secs = duration.as_secs_f64().max(1.0);
        let buffer_gap = applied.target_surb_buffer_size.abs_diff(target.target_surb_buffer_size);
        let rate_gap = applied.max_surbs_per_sec.abs_diff(target.max_surbs_per_sec);
        Self {
            buffer_per_sec: (buffer_gap as f64 / secs).ceil() as u64,
            rate_per_sec: (rate_gap as f64 / secs).ceil() as u64,
        }
    }
}

/// Move a single value toward `target` by at most `rate * elapsed`, without overshoot.
fn step_towards(current: u64, target: u64, rate_per_sec: u64, elapsed: Duration) -> u64 {
    let max_step = (rate_per_sec as f64 * elapsed.as_secs_f64()) as u64;
    if current < target {
        current.saturating_add(max_step).min(target)
    } else {
        current.saturating_sub(max_step).max(target)
    }
}

/// Moves `applied`'s capacity fields toward `target` by at most `rate`, never overshooting; decay/sustain flags come from `target` as-is.
pub(crate) fn slew_towards(
    applied: SurbBalancerConfig,
    target: SurbBalancerConfig,
    elapsed: Duration,
    rate: SurbSlewRate,
) -> SurbBalancerConfig {
    SurbBalancerConfig {
        target_surb_buffer_size: step_towards(
            applied.target_surb_buffer_size,
            target.target_surb_buffer_size,
            rate.buffer_per_sec,
            elapsed,
        ),
        max_surbs_per_sec: step_towards(
            applied.max_surbs_per_sec,
            target.max_surbs_per_sec,
            rate.rate_per_sec,
            elapsed,
        ),
        surb_decay: target.surb_decay,
        sustain_on_return_path_loss: target.sustain_on_return_path_loss,
    }
}

#[derive(Debug)]
pub enum Setback {
    OpenBridge(String),
    RegisterWg(String),
    OpenPing(String),
    Ping(String),
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Hopr error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Gvpn client error: {0}")]
    GvpnClient(#[from] gvpn_client::Error),
    #[error("Ping error: {0}")]
    Ping(String),
    #[error("Critical error: {0}")]
    Runtime(String),
    #[error("Surb config error: {0}")]
    SurbConfig(#[from] SurbConfigError),
    #[error("Routing error: {0}")]
    Routing(String),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("Remote data error: {0}")]
    RemoteData(#[from] remote_data::Error),
}

/// Contains stateful data of establishing a VPN connection to a destination.
/// The state transition runner for this struct is in `core::connection::up::runner`.
/// This decision was made to keep all relevant application state accessible in `core`.
/// And avoid duplicating structs in both `core` and `connection` modules.
#[derive(Clone, Debug)]
pub struct Up {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wireguard: Option<WireGuard>,
    pub registration: Option<Registration>,
    /// Temporary bridge session used during key registration; cleared once the background close completes.
    pub bridge_session: Option<SessionClientMetadata>,
    /// The ping session while connecting, promoted to Main once connected.
    pub ping_session: Option<(SessionKind, SessionClientMetadata)>,
    /// Bounded rolling window of tunnel telemetry, oldest first. Owned directly
    /// by `core` (a single-threaded actor), so no lock is needed. Reset per
    /// connection attempt since `Up` itself is freshly constructed per attempt.
    pub wg_stats: VecDeque<TunnelStatsSample>,
    /// Handle to adjust the active session's SURB balancer from telemetry.
    /// Retained past the initial ping->main adjustment so it can be reused.
    pub session_configurator: Option<HoprSessionConfigurator>,
    /// Desired SURB balancer setpoint; persists past convergence so a future demand-driven policy can retarget it.
    pub surb_target: Option<SurbBalancerConfig>,
    /// SURB balancer setpoint actually pushed to the session so far.
    pub surb_applied: Option<SurbBalancerConfig>,
    surb_ramp_rate: Option<SurbSlewRate>,
    surb_last_tick: Option<SystemTime>,
    /// Demand-driven SURB target policy state (Problem 2): smoothed WireGuard
    /// byte rates plus hysteresis bookkeeping, used to decide whether to boost
    /// `surb_target` above the main tier's baseline. `None` until observed.
    demand: Option<demand::DemandTracker>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Phase {
    Init,
    ResolvingBlokliIps,
    GeneratingWg,
    OpeningBridge,
    RegisterWg,
    OpeningPing,
    GatherPeerIps,
    KillswitchLockdown,
    EstablishWgTunnel,
    VerifyPing,
    AdjustToMain,
    ConnectionEstablished,
}

impl Error {
    pub fn is_ping_error(&self) -> bool {
        matches!(self, Error::Ping(_))
    }
}

impl Up {
    pub fn new(destination: Destination) -> Self {
        Self {
            destination,
            phase: (SystemTime::now(), Phase::Init),
            wireguard: None,
            registration: None,
            bridge_session: None,
            ping_session: None,
            wg_stats: VecDeque::new(),
            session_configurator: None,
            surb_target: None,
            surb_applied: None,
            surb_ramp_rate: None,
            surb_last_tick: None,
            demand: None,
        }
    }

    /// Advances the SURB balancer setpoint one tick toward `surb_target`; a no-op once converged, and logs rather than propagates a failed push so the next tick retries.
    pub fn advance_surb_ramp(&mut self, configurator: &HoprSessionConfigurator, now: SystemTime) {
        let (Some(target), Some(applied), Some(rate)) = (self.surb_target, self.surb_applied, self.surb_ramp_rate)
        else {
            return;
        };
        if applied == target {
            return;
        }
        let elapsed = self
            .surb_last_tick
            .and_then(|last| now.duration_since(last).ok())
            .unwrap_or_default();
        let next = slew_towards(applied, target, elapsed, rate);
        if next == applied {
            return;
        }
        match configurator.update_surb_balancer_config(next) {
            Ok(()) => {
                self.surb_applied = Some(next);
                self.surb_last_tick = Some(now);
            }
            Err(e) => tracing::warn!(error = ?e, "failed to adjust surb balancer - will retry next tick"),
        }
    }

    /// Retargets the SURB balancer's setpoint to `target`, recomputing the
    /// follower's ramp rate so it converges within `duration` regardless of
    /// the gap from whatever is currently applied - reusing the previous
    /// rate here would make convergence time an unpredictable function of
    /// whatever gap it was originally sized for (see `Progress::SetSurbTarget`).
    /// Leaves `surb_applied`/`surb_last_tick` alone: `advance_surb_ramp`
    /// already tracks incremental progress from wherever `surb_applied` sits.
    pub fn retarget_surb_balancer(&mut self, target: SurbBalancerConfig, duration: Duration) {
        if self.surb_target == Some(target) {
            return;
        }
        let applied = self.surb_applied.unwrap_or(target);
        self.surb_ramp_rate = Some(SurbSlewRate::to_cover(applied, target, duration));
        self.surb_target = Some(target);
    }

    /// Demand-driven SURB target policy (Problem 2): folds the latest
    /// `wg_stats` sample into a smoothed demand signal and retargets
    /// `surb_target` relative to `main_baseline` when sustained one-sided
    /// traffic is detected. No-op until the ping->main `SetSurbTarget`
    /// transition has already happened, so this can never disturb that
    /// bootstrap.
    pub fn maybe_adjust_surb_demand(&mut self, main_baseline: SurbBalancerConfig, now: SystemTime) {
        if self.surb_target.is_none() {
            return;
        }
        let mut recent = self.wg_stats.iter().rev();
        let Some((cur, prev)) = recent.next().zip(recent.next()) else {
            return;
        };
        let (cur, prev) = (cur.clone(), prev.clone());

        let min_demand = main_baseline.max_surbs_per_sec as f64
            * edgli::hopr_lib::exports::transport::SURB_SIZE as f64
            * demand::MIN_DEMAND_FRACTION;
        let tracker = self.demand.get_or_insert_with(|| demand::DemandTracker::new(now));
        tracker.observe(&prev, &cur, min_demand);

        let target = demand::target_for(main_baseline, tracker.is_boosted());
        self.retarget_surb_balancer(target, demand::RAMP_DURATION);
    }

    /// Record a new WireGuard telemetry sample, evicting the oldest once at
    /// the retention bound.
    pub fn record_wg_stats(&mut self, sample: TunnelStatsSample) {
        if self.wg_stats.len() >= wg_tunnel::HISTORY_CAPACITY {
            self.wg_stats.pop_front();
        }
        self.wg_stats.push_back(sample);
    }

    pub fn connect_progress(&mut self, evt: Box<Progress>) {
        let now = SystemTime::now();
        match *evt {
            Progress::ResolveBlokliIps => self.phase = (now, Phase::ResolvingBlokliIps),
            Progress::GenerateWg(_) => self.phase = (now, Phase::GeneratingWg),
            Progress::OpenBridge(wg) => {
                self.phase = (now, Phase::OpeningBridge);
                self.wireguard = Some(wg);
            }
            Progress::BridgeOpened(meta) => {
                self.bridge_session = Some(meta);
            }
            Progress::RegisterWg => self.phase = (now, Phase::RegisterWg),
            Progress::OpenPing(reg) => {
                self.phase = (now, Phase::OpeningPing);
                self.registration = Some(reg);
            }
            Progress::BridgeClosed => {
                self.bridge_session = None;
            }
            Progress::PeerIps => self.phase = (now, Phase::GatherPeerIps),
            Progress::KillswitchLockdown => self.phase = (now, Phase::KillswitchLockdown),
            Progress::StaticWgTunnel(session) => {
                self.phase = (now, Phase::EstablishWgTunnel);
                self.ping_session = Some((SessionKind::Ping, session));
            }
            Progress::SessionConfigurator(configurator) => {
                self.session_configurator = Some(configurator);
            }
            Progress::Ping => self.phase = (now, Phase::VerifyPing),
            Progress::AdjustToMain(_round_trip_time) => self.phase = (now, Phase::AdjustToMain),
            Progress::SetSurbTarget { applied, target } => {
                self.surb_ramp_rate = Some(SurbSlewRate::to_cover(applied, target, SURB_RAMP_DURATION));
                self.surb_applied = Some(applied);
                self.surb_target = Some(target);
                self.surb_last_tick = Some(now);
            }
        }
    }

    pub fn connected(&mut self) {
        self.phase = (SystemTime::now(), Phase::ConnectionEstablished);
        if let Some((SessionKind::Ping, meta)) = self.ping_session.take() {
            self.ping_session = Some((SessionKind::Main, meta));
        }
    }
}

impl Display for Up {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Connection to {} ({:?} since {})",
            self.destination,
            self.phase.1,
            log_output::elapsed(&self.phase.0)
        )
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase_str = match self {
            Phase::Init => "Init",
            Phase::ResolvingBlokliIps => "Resolving Blokli IPs",
            Phase::GeneratingWg => "Generating WireGuard keypairs",
            Phase::OpeningBridge => "Opening bridge connection",
            Phase::RegisterWg => "Registering WireGuard public key",
            Phase::OpeningPing => "Opening main connection",
            Phase::GatherPeerIps => "Retrieving peer IPs",
            Phase::KillswitchLockdown => "Activating killswitch",
            Phase::EstablishWgTunnel => "Establishing WireGuard tunnel",
            Phase::VerifyPing => "Verifying established connection",
            Phase::AdjustToMain => "Upgrading for general traffic",
            Phase::ConnectionEstablished => "Connection established",
        };
        write!(f, "{}", phase_str)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Progress(p) => write!(f, "Progress: {p}"),
            Event::Setback(s) => write!(f, "Setback: {s}"),
        }
    }
}

impl Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Progress::ResolveBlokliIps => write!(f, "Resolving Blokli IPs"),
            Progress::GenerateWg(_) => write!(f, "Generating WireGuard keypairs"),
            Progress::OpenBridge(_) => write!(f, "Opening bridge connection"),
            Progress::BridgeOpened(_) => write!(f, "Bridge session opened"),
            Progress::RegisterWg => write!(f, "Registering WireGuard public key"),
            Progress::OpenPing(_) => write!(f, "Opening main connection"),
            Progress::BridgeClosed => write!(f, "Bridge session closed"),
            Progress::PeerIps => write!(f, "Retrieving peer IPs"),
            Progress::KillswitchLockdown => write!(f, "Activating killswitch"),
            Progress::StaticWgTunnel(_) => write!(f, "Establishing static WireGuard tunnel"),
            Progress::SessionConfigurator(_) => write!(f, "Session configurator retained"),
            Progress::Ping => write!(f, "Verifying established connection"),
            Progress::AdjustToMain(round_trip_time) => {
                write!(f, "Adjusting to main connection with RTT of {:?}", round_trip_time)
            }
            Progress::SetSurbTarget { .. } => write!(f, "Ramping SURB balancer to main session"),
        }
    }
}

impl Display for Setback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Setback::OpenBridge(err) => write!(f, "Failed to open bridge connection: {err}"),
            Setback::RegisterWg(err) => write!(f, "Failed to register WireGuard key: {err}"),
            Setback::OpenPing(err) => write!(f, "Failed to open main connection: {err}"),
            Setback::Ping(err) => write!(f, "Ping verification failed: {err}"),
        }
    }
}

#[cfg(test)]
mod surb_ramp_tests {
    use super::*;

    fn config(buffer: u64, rate: u64) -> SurbBalancerConfig {
        SurbBalancerConfig {
            target_surb_buffer_size: buffer,
            max_surbs_per_sec: rate,
            ..Default::default()
        }
    }

    #[test]
    fn step_towards_does_not_move_when_elapsed_is_zero() {
        assert_eq!(step_towards(0, 100, 10, Duration::ZERO), 0);
    }

    #[test]
    fn step_towards_does_not_move_when_already_at_target() {
        assert_eq!(step_towards(100, 100, 10, Duration::from_secs(5)), 100);
    }

    #[test]
    fn step_towards_moves_up_without_overshoot() {
        assert_eq!(step_towards(0, 100, 10, Duration::from_secs(5)), 50);
        // A large elapsed converges exactly on the target, never past it.
        assert_eq!(step_towards(0, 100, 10, Duration::from_secs(50)), 100);
    }

    #[test]
    fn step_towards_moves_down_without_overshoot() {
        // Problem 2's future decay-back-down case: the same function, reversed.
        assert_eq!(step_towards(100, 0, 10, Duration::from_secs(5)), 50);
        assert_eq!(step_towards(100, 0, 10, Duration::from_secs(50)), 0);
    }

    #[test]
    fn slew_towards_keeps_target_decay_and_sustain_flags() {
        let applied = config(0, 0);
        let target = SurbBalancerConfig {
            surb_decay: Some((Duration::from_secs(60), 0.05)),
            sustain_on_return_path_loss: true,
            ..config(1000, 100)
        };
        let rate = SurbSlewRate::to_cover(applied, target, Duration::from_secs(10));
        let next = slew_towards(applied, target, Duration::from_secs(10), rate);
        assert_eq!(next, target, "a full-duration step should land exactly on the target");
    }

    #[test]
    fn surb_slew_rate_to_cover_closes_the_gap_over_the_given_duration() {
        let applied = config(0, 0);
        let target = config(600, 60);
        let rate = SurbSlewRate::to_cover(applied, target, Duration::from_secs(60));
        let halfway = slew_towards(applied, target, Duration::from_secs(30), rate);
        assert_eq!(halfway, config(300, 30));
        let done = slew_towards(applied, target, Duration::from_secs(60), rate);
        assert_eq!(done, target);
    }
}
