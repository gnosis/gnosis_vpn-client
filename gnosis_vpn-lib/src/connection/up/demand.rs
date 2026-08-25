use edgli::hopr_lib::exports::transport::SurbBalancerConfig;

use std::time::{Duration, SystemTime};

use crate::wg_tunnel::TunnelStatsSample;

/// EWMA time constant: filters ~4s per-sample noise while staying well above
/// hoprd's own 100ms PID cadence - the "outer, slow" half of the system.
const EWMA_TAU: Duration = Duration::from_secs(30);
/// Floor below which a direction's demand is ignored as idle-link noise, as a
/// fraction of the main tier's configured throughput ceiling - self-scales
/// with whatever bandwidth the operator has configured.
pub(crate) const MIN_DEMAND_FRACTION: f64 = 0.1;
/// Dominant/recessive ratio required to enter the boosted state.
const ENTER_IMBALANCE_RATIO: f64 = 4.0;
/// Ratio to *exit* boosted - lower than enter, so hovering near a threshold can't flip every tick.
const EXIT_IMBALANCE_RATIO: f64 = 2.0;
/// Minimum dwell time before a state is allowed to flip again - a second, independent thrash guard.
const MIN_DWELL: Duration = Duration::from_secs(45);
/// How long a demand-driven retarget takes to converge - shorter than the
/// startup ramp's 60s since these gaps are smaller and should arrive promptly.
pub(crate) const RAMP_DURATION: Duration = Duration::from_secs(20);
const BOOST_MULTIPLIER: u64 = 2;
/// Independent safety cap (~double the main tier's own documented 10 MB
/// "maximum allowed" default) since the client can't see the exit node's real ceiling.
const ABSOLUTE_CAP_SURBS: u64 = 20_000;

/// Smoothed WireGuard byte rates plus hysteresis bookkeeping, used to decide
/// whether sustained one-sided traffic warrants boosting the SURB target.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DemandTracker {
    tx_ewma_bytes_per_sec: f64,
    rx_ewma_bytes_per_sec: f64,
    boosted: bool,
    state_since: SystemTime,
}

impl DemandTracker {
    pub(crate) fn new(now: SystemTime) -> Self {
        Self {
            tx_ewma_bytes_per_sec: 0.0,
            rx_ewma_bytes_per_sec: 0.0,
            boosted: false,
            state_since: now,
        }
    }

    pub(crate) fn is_boosted(&self) -> bool {
        self.boosted
    }

    /// Folds a new sample pair into the smoothed rates and re-evaluates the boosted state.
    pub(crate) fn observe(&mut self, prev: &TunnelStatsSample, cur: &TunnelStatsSample, min_demand_bytes_per_sec: f64) {
        let elapsed = cur.at.duration_since(prev.at).unwrap_or_default();
        if elapsed.is_zero() {
            return;
        }
        let secs = elapsed.as_secs_f64();
        // TODO: replace this WG tx/rx-byte-delta proxy with hoprd's own SURB-balancer
        // buffer-level telemetry (`hopr_surb_balancer_current_buffer_estimate` /
        // `current_buffer_target`, reachable via `hopr::telemetry()`) once edge-client
        // stabilizes exporting it; that reflects the real buffer level, not an inference.
        let tx_rate = cur.tx_bytes.saturating_sub(prev.tx_bytes) as f64 / secs;
        let rx_rate = cur.rx_bytes.saturating_sub(prev.rx_bytes) as f64 / secs;

        let alpha = 1.0 - (-secs / EWMA_TAU.as_secs_f64()).exp();
        self.tx_ewma_bytes_per_sec += alpha * (tx_rate - self.tx_ewma_bytes_per_sec);
        self.rx_ewma_bytes_per_sec += alpha * (rx_rate - self.rx_ewma_bytes_per_sec);

        self.maybe_flip(cur.at, min_demand_bytes_per_sec);
    }

    fn maybe_flip(&mut self, now: SystemTime, min_demand_bytes_per_sec: f64) {
        let dominant = self.tx_ewma_bytes_per_sec.max(self.rx_ewma_bytes_per_sec);
        let recessive = self.tx_ewma_bytes_per_sec.min(self.rx_ewma_bytes_per_sec);
        let should = wants_boost(dominant, recessive, self.boosted, min_demand_bytes_per_sec);
        let dwelled = now.duration_since(self.state_since).unwrap_or_default() >= MIN_DWELL;
        if should != self.boosted && dwelled {
            self.boosted = should;
            self.state_since = now;
        }
    }
}

/// Whether the smoothed traffic looks like sustained one-sided demand.
/// Uses a lower ratio to stay boosted than to enter it (hysteresis), so a
/// signal hovering near the threshold can't flip back and forth.
fn wants_boost(dominant: f64, recessive: f64, currently_boosted: bool, min_demand_bytes_per_sec: f64) -> bool {
    if dominant < min_demand_bytes_per_sec {
        return false;
    }
    let ratio = if currently_boosted {
        EXIT_IMBALANCE_RATIO
    } else {
        ENTER_IMBALANCE_RATIO
    };
    dominant >= recessive * ratio
}

/// Doubles `baseline`'s target buffer size when boosted, capped at an
/// absolute ceiling independent of the multiplier. Only `target_surb_buffer_size`
/// moves; rate cap, decay, and sustain-on-loss pass through from `baseline` unchanged.
/// Decay back to `baseline` is free: once `boosted` is false this returns
/// `baseline` again, and the untouched follower slews back down on its own.
pub(crate) fn target_for(baseline: SurbBalancerConfig, boosted: bool) -> SurbBalancerConfig {
    if !boosted {
        return baseline;
    }
    let size = baseline
        .target_surb_buffer_size
        .saturating_mul(BOOST_MULTIPLIER)
        .min(ABSOLUTE_CAP_SURBS)
        .max(baseline.target_surb_buffer_size);
    SurbBalancerConfig {
        target_surb_buffer_size: size,
        ..baseline
    }
}

#[cfg(test)]
mod demand_tests {
    use super::*;

    fn config(buffer: u64) -> SurbBalancerConfig {
        SurbBalancerConfig {
            target_surb_buffer_size: buffer,
            max_surbs_per_sec: 100,
            ..Default::default()
        }
    }

    fn sample(at_secs: u64, tx_bytes: u64, rx_bytes: u64) -> TunnelStatsSample {
        TunnelStatsSample {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(at_secs),
            tx_bytes,
            rx_bytes,
            rtt_ms: None,
            time_since_last_handshake: None,
            estimated_loss: 0.0,
        }
    }

    const MIN_DEMAND: f64 = 1000.0;

    #[test]
    fn wants_boost_requires_minimum_absolute_rate() {
        // 100:1 ratio, but dominant is below the floor.
        assert!(!wants_boost(500.0, 5.0, false, MIN_DEMAND));
    }

    #[test]
    fn wants_boost_enters_on_strong_imbalance() {
        assert!(wants_boost(10_000.0, 1_000.0, false, MIN_DEMAND));
    }

    #[test]
    fn wants_boost_does_not_enter_on_mild_imbalance() {
        assert!(!wants_boost(1_500.0, 1_000.0, false, MIN_DEMAND));
    }

    #[test]
    fn wants_boost_hysteresis_keeps_boosted_state_at_a_ratio_that_would_not_enter_it() {
        // 3:1 sits between EXIT_IMBALANCE_RATIO (2.0) and ENTER_IMBALANCE_RATIO (4.0).
        let dominant = 3_000.0;
        let recessive = 1_000.0;
        assert!(!wants_boost(dominant, recessive, false, MIN_DEMAND));
        assert!(wants_boost(dominant, recessive, true, MIN_DEMAND));
    }

    #[test]
    fn target_for_returns_baseline_when_not_boosted() {
        let baseline = config(1000);
        assert_eq!(target_for(baseline, false), baseline);
    }

    #[test]
    fn target_for_doubles_baseline_when_boosted_and_under_cap() {
        let baseline = config(1000);
        assert_eq!(target_for(baseline, true).target_surb_buffer_size, 2000);
    }

    #[test]
    fn target_for_clamps_to_absolute_cap() {
        let baseline = config(15_000);
        assert_eq!(target_for(baseline, true).target_surb_buffer_size, ABSOLUTE_CAP_SURBS);
    }

    #[test]
    fn target_for_never_drops_below_baseline() {
        let baseline = config(25_000); // already above ABSOLUTE_CAP_SURBS
        assert_eq!(target_for(baseline, true).target_surb_buffer_size, 25_000);
    }

    #[test]
    fn sustained_one_sided_download_triggers_boost() {
        let mut tracker = DemandTracker::new(SystemTime::UNIX_EPOCH);
        let mut prev = sample(0, 0, 0);
        let mut boosted_at = None;
        for i in 1..=20u64 {
            let cur = sample(i * 4, i * 40, i * 4_000_000);
            tracker.observe(&prev, &cur, MIN_DEMAND);
            if tracker.is_boosted() && boosted_at.is_none() {
                boosted_at = Some(i);
            }
            prev = cur;
        }
        assert!(boosted_at.is_some(), "sustained heavy download should eventually boost");
        assert!(
            boosted_at.unwrap() * 4 >= 45,
            "must not boost before MIN_DWELL has elapsed since tracker creation"
        );
    }

    #[test]
    fn transient_spike_does_not_trigger_boost() {
        let mut tracker = DemandTracker::new(SystemTime::UNIX_EPOCH);
        let samples = [
            sample(0, 0, 0),
            sample(4, 10, 4_000_000), // one brief heavy-download sample
            sample(8, 20, 4_000_100), // back to balanced
            sample(12, 30, 4_000_200),
        ];
        for pair in samples.windows(2) {
            tracker.observe(&pair[0], &pair[1], MIN_DEMAND);
            assert!(!tracker.is_boosted(), "a brief spike must not trigger a boost");
        }
    }

    #[test]
    fn demand_subsiding_decays_target_back_to_baseline() {
        let mut tracker = DemandTracker::new(SystemTime::UNIX_EPOCH);
        let mut prev = sample(0, 0, 0);
        for i in 1..=20u64 {
            let cur = sample(i * 4, i * 40, i * 4_000_000);
            tracker.observe(&prev, &cur, MIN_DEMAND);
            prev = cur;
        }
        assert!(
            tracker.is_boosted(),
            "precondition: should be boosted before demand subsides"
        );

        // Traffic returns to balanced and stays there long enough for the EWMA
        // (tau=30s) to decay the huge boost-phase rx rate back down - many
        // time constants, since it started from a large ratio.
        let start = 20 * 4;
        for i in 1..=150u64 {
            let t = start + i * 4;
            let cur = sample(t, prev.tx_bytes + i * 40, prev.rx_bytes + i * 40);
            tracker.observe(&prev, &cur, MIN_DEMAND);
            prev = cur;
        }
        assert!(
            !tracker.is_boosted(),
            "sustained balanced traffic should decay the boost"
        );

        let baseline = config(1000);
        assert_eq!(target_for(baseline, tracker.is_boosted()), baseline);
    }

    #[test]
    fn noisy_borderline_sequence_does_not_thrash() {
        let mut tracker = DemandTracker::new(SystemTime::UNIX_EPOCH);
        let mut prev = sample(0, 0, 0);
        let mut transitions = 0;
        let mut last_boosted = tracker.is_boosted();
        // Alternate between two rates whose EWMA hovers right around the
        // enter/exit ratio band, spaced well under MIN_DWELL apart.
        for i in 1..=30u64 {
            let rx = if i % 2 == 0 { i * 4_000_000 } else { i * 1_500_000 };
            let cur = sample(i * 4, i * 1_000_000, rx);
            tracker.observe(&prev, &cur, MIN_DEMAND);
            if tracker.is_boosted() != last_boosted {
                transitions += 1;
                last_boosted = tracker.is_boosted();
            }
            prev = cur;
        }
        assert!(
            transitions <= 1,
            "noisy borderline traffic must not thrash the boosted state"
        );
    }
}
