pub use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use crate::serde_utils;

use std::collections::HashMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

/// wxHOPR amounts (in whole tokens, i.e. the value returned by
/// `Balance::amount_in_base_units` after the wei→token conversion) below this are
/// awkward to read in plain decimal, so we additionally surface them in scientific
/// notation. `1e-3` here means 0.001 wxHOPR, not wei.
const WXHOPR_SCI_THRESHOLD: f64 = 1e-3;

/// Scientific-notation form of a wxHOPR balance (e.g. `7.5e-10`), but only for values
/// small enough that the decimal form is hard to read. Returns `None` for zero and for
/// amounts at or above `1e-3` wxHOPR (the token value, already converted from wei),
/// where the decimal form is already legible.
pub fn wxhopr_scientific(b: Balance<WxHOPR>) -> Option<String> {
    let v: f64 = b.amount_in_base_units().parse().ok()?;
    (v > 0.0 && v < WXHOPR_SCI_THRESHOLD).then(|| {
        // round to 2 decimal places, then drop trailing zeros (and a bare
        // trailing `.`) from the mantissa for readability: `1.00e-18` -> `1e-18`,
        // `7.50e-10` -> `7.5e-10`. Only the mantissa is trimmed, never the exponent.
        let s = format!("{v:.2e}");
        match s.split_once('e') {
            Some((mantissa, exp)) => {
                let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
                format!("{mantissa}e{exp}")
            }
            None => s,
        }
    })
}

/// Traffic/gas health, pooled across all allocations rather than checked per-location.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingLevel {
    Good,
    Low,
    Empty,
}

/// Traffic/gas health plus wxHOPR/xDAI still needed to reach the ideal recommendation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundingStatus {
    pub traffic: FundingLevel,
    pub gas: FundingLevel,
    /// wxHOPR still needed to reach the ideal recommendation; can be `None` even while `traffic` isn't `Good`.
    #[serde(with = "serde_utils::opt_balance")]
    pub wxhopr_deficit: Option<Balance<WxHOPR>>,
    /// xDAI still needed to reach the ideal recommendation; `None` while `gas` is `Good`.
    #[serde(with = "serde_utils::opt_balance")]
    pub xdai_deficit: Option<Balance<XDai>>,
}

impl Display for FundingLevel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            FundingLevel::Good => "Good",
            FundingLevel::Low => "Low",
            FundingLevel::Empty => "Empty",
        };
        write!(f, "{s}")
    }
}

// Traffic bands, inclusive upper bounds: Empty at or below 768 MB, Low at or
// below 1536 MB, Good above that.
const TRAFFIC_EMPTY_MAX_BYTES: u64 = 768 * 1024 * 1024;
const TRAFFIC_LOW_MAX_BYTES: u64 = 1536 * 1024 * 1024;
// 0.0015 / 0.0035 xDAI, in wei
const XDAI_EMPTY_BELOW_WEI: u64 = 1_500_000_000_000_000;
const XDAI_LOW_BELOW_WEI: u64 = 3_500_000_000_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingTool {
    NotStarted,
    InProgress,
    CompletedSuccess(#[serde(with = "serde_utils::system_time")] SystemTime),
    CompletedError(String),
}

impl FundingTool {
    /// Successful runs are only restartable once this much time has passed, so a
    /// misclick can't immediately re-trigger an on-chain funding transaction.
    const RERUN_COOLDOWN: Duration = Duration::from_secs(5 * 60);

    /// Time left before a successful run may be restarted, or `None` if it's not
    /// cooling down (still in progress, never run, errored, or cooldown elapsed).
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let FundingTool::CompletedSuccess(completed_at) = self else {
            return None;
        };
        let elapsed = completed_at.elapsed().unwrap_or_default();
        Self::RERUN_COOLDOWN
            .checked_sub(elapsed)
            .filter(|remaining| !remaining.is_zero())
    }
}

/// Data-throughput capacity for a wxHOPR stake at the current ticket price.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Capacity {
    #[serde(with = "serde_utils::balance")]
    pub stake: Balance<WxHOPR>,
    pub expected_messages: u64,
    pub min_guaranteed_messages: u64,
    pub byte_capacity: u64,
}

impl Default for Capacity {
    fn default() -> Self {
        Capacity {
            stake: Balance::<WxHOPR>::zero(),
            expected_messages: 0,
            min_guaranteed_messages: 0,
            byte_capacity: 0,
        }
    }
}

/// Serde mirror of [`edgli::strategy::CapacityAllocations`]: every entity holding
/// a wxHOPR stake — open outgoing channels, the unallocated Safe balance, and the
/// node EOA (deposited funds not yet swept into the Safe).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapacityAllocations {
    /// Open outgoing payment channels, keyed by destination peer.
    #[serde(with = "serde_utils::address_map")]
    pub peer_allocations: HashMap<Address, Capacity>,
    /// wxHOPR on the node EOA, not yet swept into the Safe.
    pub node: Capacity,
    /// The unallocated wxHOPR balance held in the user's Safe contract.
    pub safe: Capacity,
}

impl From<edgli::strategy::Capacity> for Capacity {
    fn from(c: edgli::strategy::Capacity) -> Self {
        Capacity {
            stake: c.stake,
            expected_messages: c.expected_messages,
            min_guaranteed_messages: c.min_guaranteed_messages,
            byte_capacity: c.byte_capacity,
        }
    }
}

impl From<edgli::strategy::CapacityAllocations> for CapacityAllocations {
    fn from(a: edgli::strategy::CapacityAllocations) -> Self {
        CapacityAllocations {
            peer_allocations: a
                .peer_allocations
                .into_iter()
                .map(|(addr, c)| (addr, c.into()))
                .collect(),
            node: a.node.into(),
            safe: a.safe.into(),
        }
    }
}

impl Display for CapacityAllocations {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "CapacityAllocations(node: {}, safe: {}, {} channels totalling {})",
            self.node.stake,
            self.safe.stake,
            self.peer_allocations.len(),
            self.peer_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>()
        )
    }
}

/// How many successive capacity polls an unexplained Safe/EOA stake drop keeps being
/// folded back into the published total before it is accepted as a real spend.
const PENDING_ALLOCATION_MAX_POLLS: u8 = 5;

/// wxHOPR believed to be in flight between allocation locations (Safe/EOA → channel),
/// folded back into the published Safe capacity until it reappears or expires.
#[derive(Clone, Copy, Debug)]
struct PendingAllocation {
    stake: Balance<WxHOPR>,
    expected_messages: u64,
    min_guaranteed_messages: u64,
    byte_capacity: u64,
    polls_left: u8,
}

/// Pooled totals of a raw snapshot, kept between polls to detect stake drops.
#[derive(Clone, Copy, Debug)]
struct SnapshotTotals {
    stake: Balance<WxHOPR>,
    channel_stake: Balance<WxHOPR>,
    expected_messages: u64,
    min_guaranteed_messages: u64,
    byte_capacity: u64,
}

impl SnapshotTotals {
    fn of(caps: &CapacityAllocations) -> Self {
        let channel_stake = caps.peer_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>();
        let sum = |f: fn(&Capacity) -> u64| {
            f(&caps.node) + f(&caps.safe) + caps.peer_allocations.values().map(f).sum::<u64>()
        };
        SnapshotTotals {
            stake: caps.node.stake + caps.safe.stake + channel_stake,
            channel_stake,
            expected_messages: sum(|c| c.expected_messages),
            min_guaranteed_messages: sum(|c| c.min_guaranteed_messages),
            byte_capacity: sum(|c| c.byte_capacity),
        }
    }
}

/// Keeps the pooled capacity total conserved across the snapshot's non-atomic reads.
///
/// The snapshot reads the channel list (indexer-fed, lags the chain) before the live
/// Safe/EOA balance queries, so while the strategy funds a channel out of the Safe the
/// stake is counted nowhere for a poll or two — the pooled total visibly dips and
/// recovers. wxHOPR is conserved, and the only legitimate *fast* drop of Safe+EOA
/// stake is a transfer toward a channel (usage drainage hits channel stakes instead),
/// so a Safe/EOA drop not matched by a channel gain is treated as in-flight and folded
/// back into the published Safe capacity. The fold-in expires after
/// [`PENDING_ALLOCATION_MAX_POLLS`] polls so a real spend (e.g. a manual withdrawal)
/// still surfaces, just late; the same window means a transiently over-counted
/// snapshot (EOA→Safe sweep landing between the two balance reads) also decays over
/// that many polls instead of one.
#[derive(Debug, Default)]
pub struct CapacityReconciler {
    prev: Option<SnapshotTotals>,
    pending: Option<PendingAllocation>,
}

impl CapacityReconciler {
    /// Feed a fresh raw snapshot and get the snapshot to publish, with any in-flight
    /// stake folded into the `safe` component (values only — same shape, so downstream
    /// consumers summing the components are covered without protocol changes).
    pub fn reconcile(&mut self, raw: CapacityAllocations) -> CapacityAllocations {
        let totals = SnapshotTotals::of(&raw);
        let pending_stake = match &self.prev {
            Some(prev) => {
                let carried = self.pending.map(|p| p.stake).unwrap_or_else(Balance::zero);
                // Channel-stake drops (ticket drainage, closure) are real and pass
                // through; `-` saturates at zero, yielding
                // max(0, published_prev - channel_drop - raw_total).
                let channel_drop = prev.channel_stake - totals.channel_stake;
                prev.stake + carried - channel_drop - totals.stake
            }
            None => Balance::zero(),
        };

        self.pending = self.next_pending(pending_stake, &totals);
        self.prev = Some(totals);

        let mut published = raw;
        if let Some(p) = &self.pending {
            tracing::info!(stake = %p.stake, polls_left = p.polls_left, "counting in-flight stake toward published capacity");
            published.safe.stake += p.stake;
            published.safe.expected_messages += p.expected_messages;
            published.safe.min_guaranteed_messages += p.min_guaranteed_messages;
            published.safe.byte_capacity += p.byte_capacity;
        }
        published
    }

    fn next_pending(&self, stake: Balance<WxHOPR>, current: &SnapshotTotals) -> Option<PendingAllocation> {
        if stake.is_zero() {
            return None;
        }
        let polls_left = match &self.pending {
            // a further drop restarts the clock; otherwise keep counting down
            Some(old) if stake > old.stake => PENDING_ALLOCATION_MAX_POLLS,
            Some(old) => old.polls_left.saturating_sub(1),
            None => PENDING_ALLOCATION_MAX_POLLS,
        };
        if polls_left == 0 {
            tracing::warn!(%stake, "stake drop never resolved into a channel - accepting the lower total");
            return None;
        }
        let (byte_capacity, expected_messages, min_guaranteed_messages) = self.scaled_capacity(stake, current);
        Some(PendingAllocation {
            stake,
            expected_messages,
            min_guaranteed_messages,
            byte_capacity,
            polls_left,
        })
    }

    /// Capacity numbers for a pending stake, scaled linearly from the freshest
    /// stake→capacity ratio available (the client holds neither ticket price nor win
    /// probability, so it cannot recompute capacities from scratch). Whenever a
    /// pending stake exists, at least one fallback has a non-zero stake to scale from.
    fn scaled_capacity(&self, stake: Balance<WxHOPR>, current: &SnapshotTotals) -> (u64, u64, u64) {
        let tokens = |b: Balance<WxHOPR>| -> f64 {
            b.amount_in_base_units().parse().unwrap_or_else(|e| {
                tracing::warn!(balance = %b, error = %e, "failed to parse balance while scaling pending capacity");
                0.0
            })
        };
        let scale = |bytes: u64, msgs: u64, min_msgs: u64, base: Balance<WxHOPR>| {
            let base = tokens(base);
            (base > 0.0).then(|| {
                let r = tokens(stake) / base;
                (
                    (bytes as f64 * r) as u64,
                    (msgs as f64 * r) as u64,
                    (min_msgs as f64 * r) as u64,
                )
            })
        };
        let of_totals =
            |t: &SnapshotTotals| scale(t.byte_capacity, t.expected_messages, t.min_guaranteed_messages, t.stake);
        of_totals(current)
            .or_else(|| self.prev.as_ref().and_then(of_totals))
            .or_else(|| {
                self.pending
                    .as_ref()
                    .and_then(|p| scale(p.byte_capacity, p.expected_messages, p.min_guaranteed_messages, p.stake))
            })
            .unwrap_or((0, 0, 0))
    }
}

/// Recommended wxHOPR and xDAI balance to open the target number of channels.
/// `wxhopr` is the total to fund: channel stakes plus the one-time key-binding
/// (announcement) fee, which on some networks (e.g. rotsee: 0.01 wxHOPR) dwarfs the
/// channel stakes themselves. The breakdown fields mirror
/// `edgli::strategy::BalanceRecommendation`. Carries both the minimum
/// recommendation (computed once during onboarding, surfaced in the PreparingSafe
/// run mode and gating safe deployment) and the ideal recommendation (refreshed
/// periodically while running, feeding funding-issue checks and balance responses).
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct BalanceRecommendation {
    /// Total wxHOPR to fund: channel stakes plus the fee to start.
    #[serde(with = "serde_utils::balance")]
    pub wxhopr: Balance<WxHOPR>,
    /// Recommended xDai balance for gas: the total amount to fund the node with.
    ///
    /// Not [`Self::xdai_fee_per_tx`], which is a ceiling on a single transaction rather than
    /// expected spend -- funding one transaction's worth would leave the node unable to finish
    /// starting up.
    #[serde(with = "serde_utils::balance")]
    pub xdai: Balance<XDai>,
    /// wxHOPR needed to stake the missing channels.
    #[serde(with = "serde_utils::balance")]
    pub channel_stakes: Balance<WxHOPR>,
    /// One-time key-binding fee still owed before the node can start;
    /// zero once the key is bound on-chain.
    #[serde(with = "serde_utils::balance")]
    pub fee_to_start: Balance<WxHOPR>,
    /// Number of on-chain transactions still needed before channel funding can
    /// begin (Safe deployment, Safe registration, key-binding announcement).
    pub txs_to_start: u64,
    /// Maximum xDAI fee per transaction (gas).
    #[serde(with = "serde_utils::balance")]
    pub xdai_fee_per_tx: Balance<XDai>,
}

impl From<edgli::strategy::BalanceRecommendation> for BalanceRecommendation {
    fn from(rec: edgli::strategy::BalanceRecommendation) -> Self {
        BalanceRecommendation {
            wxhopr: rec.total_wxhopr(),
            xdai: rec.xdai_fund_amount,
            channel_stakes: rec.channel_stakes,
            fee_to_start: rec.fee_to_start,
            txs_to_start: rec.txs_to_start,
            xdai_fee_per_tx: rec.xdai_fee_per_tx,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreSafe {
    pub node_xdai: Balance<XDai>,
    pub node_wxhopr: Balance<WxHOPR>,
}

impl Default for PreSafe {
    fn default() -> Self {
        Self {
            node_xdai: Balance::<XDai>::zero(),
            node_wxhopr: Balance::<WxHOPR>::zero(),
        }
    }
}

impl Display for PreSafe {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(node_xdai: {}, node_wxhopr: {})", self.node_xdai, self.node_wxhopr)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balances {
    pub node_xdai: Balance<XDai>,
    pub safe_wxhopr: Balance<WxHOPR>,
    pub channels_out: HashMap<Address, Balance<WxHOPR>>,
}

impl Display for Balances {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balances(node_xdai: {}, safe_wxhopr: {}, channels_out_wxhopr: {})",
            self.node_xdai,
            self.safe_wxhopr,
            self.channels_out.values().copied().sum::<Balance<WxHOPR>>()
        )
    }
}

/// Pools every allocation location so funds sitting unswept on the node EOA still count.
pub fn to_funding_status(
    ideal: BalanceRecommendation,
    capacity_allocations: &CapacityAllocations,
    node_xdai: Balance<XDai>,
) -> FundingStatus {
    let peer_stake = capacity_allocations
        .peer_allocations
        .values()
        .map(|c| c.stake)
        .sum::<Balance<WxHOPR>>();
    let total_stake = capacity_allocations.node.stake + capacity_allocations.safe.stake + peer_stake;

    let peer_bytes: u64 = capacity_allocations
        .peer_allocations
        .values()
        .map(|c| c.byte_capacity)
        .sum();
    let total_bytes = capacity_allocations.node.byte_capacity + capacity_allocations.safe.byte_capacity + peer_bytes;

    let traffic = if total_bytes <= TRAFFIC_EMPTY_MAX_BYTES {
        FundingLevel::Empty
    } else if total_bytes <= TRAFFIC_LOW_MAX_BYTES {
        FundingLevel::Low
    } else {
        FundingLevel::Good
    };

    let xdai_empty_below = Balance::<XDai>::from(XDAI_EMPTY_BELOW_WEI);
    let xdai_low_below = Balance::<XDai>::from(XDAI_LOW_BELOW_WEI);
    let gas = if node_xdai < xdai_empty_below {
        FundingLevel::Empty
    } else if node_xdai < xdai_low_below {
        FundingLevel::Low
    } else {
        FundingLevel::Good
    };

    // `-` on Balance saturates at zero; wxhopr_deficit is relative to `ideal` only, which ignores drained stake on already-open channels.
    let wxhopr_deficit = (traffic != FundingLevel::Good)
        .then(|| ideal.wxhopr - total_stake)
        .filter(|d| !d.is_zero());
    // Floored at xdai_low_below so this can't go None while gas isn't Good, even if `ideal` dips below it.
    let xdai_deficit = (gas != FundingLevel::Good)
        .then(|| ideal.xdai.max(xdai_low_below) - node_xdai)
        .filter(|d| !d.is_zero());

    FundingStatus {
        traffic,
        gas,
        wxhopr_deficit,
        xdai_deficit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ideal(wxhopr: u64, xdai: u64) -> BalanceRecommendation {
        BalanceRecommendation {
            wxhopr: Balance::<WxHOPR>::from(wxhopr),
            xdai: Balance::<XDai>::from(xdai),
            channel_stakes: Balance::<WxHOPR>::from(wxhopr),
            fee_to_start: Balance::<WxHOPR>::zero(),
            txs_to_start: 0,
            xdai_fee_per_tx: Balance::<XDai>::from(xdai),
        }
    }

    fn capacity(stake: u64, bytes: u64) -> Capacity {
        Capacity {
            stake: Balance::<WxHOPR>::from(stake),
            expected_messages: 0,
            min_guaranteed_messages: 0,
            byte_capacity: bytes,
        }
    }

    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;

    /// Allocations with at most one open channel (to a fixed peer address).
    fn allocs(peer: Option<Capacity>, node: Capacity, safe: Capacity) -> CapacityAllocations {
        CapacityAllocations {
            peer_allocations: peer.map(|c| (Address::from([1u8; 20]), c)).into_iter().collect(),
            node,
            safe,
        }
    }

    #[test]
    fn traffic_empty_when_no_capacity_anywhere() {
        let status = to_funding_status(ideal(0, 0), &CapacityAllocations::default(), Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Empty);
    }

    #[test]
    fn traffic_pools_channel_safe_and_node_eoa_bytes() {
        // 320 MB each, pooled to 960 MB: above the 768 MB Empty bound, within Low.
        let allocations = allocs(
            Some(capacity(0, 320 * MB)),
            capacity(0, 320 * MB),
            capacity(0, 320 * MB),
        );
        let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Low);
    }

    #[test]
    fn traffic_empty_up_to_768mb_inclusive() {
        let allocations = allocs(None, capacity(0, 768 * MB), Capacity::default());
        let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Empty);
    }

    #[test]
    fn traffic_low_between_thresholds_up_to_1536mb_inclusive() {
        for bytes in [768 * MB + 1, 1536 * MB] {
            let allocations = allocs(None, capacity(0, bytes), Capacity::default());
            let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
            assert_eq!(status.traffic, FundingLevel::Low);
        }
    }

    #[test]
    fn traffic_good_above_1536mb() {
        // unswept EOA wxHOPR alone counts toward traffic.
        let allocations = allocs(None, capacity(0, 1536 * MB + 1), Capacity::default());
        let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Good);
    }

    #[test]
    fn gas_empty_below_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(1_000_000_000_000_000_u64), // 0.001 xDAI < 0.0015 threshold
        );
        assert_eq!(status.gas, FundingLevel::Empty);
    }

    #[test]
    fn gas_low_between_thresholds() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(2_000_000_000_000_000_u64), // 0.002 xDAI
        );
        assert_eq!(status.gas, FundingLevel::Low);
    }

    #[test]
    fn gas_good_at_or_above_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI, at the boundary
        );
        assert_eq!(status.gas, FundingLevel::Good);
    }

    #[test]
    fn balance_recommendation_from_edgli_totals_stakes_and_fee() {
        let rec = edgli::strategy::BalanceRecommendation {
            channel_stakes: Balance::<WxHOPR>::from(800u64),
            fee_to_start: Balance::<WxHOPR>::from(10_000u64),
            txs_to_start: 3,
            xdai_fee_per_tx: Balance::<XDai>::from(100u64),
            xdai_fund_amount: Balance::<XDai>::from(5_000u64),
        };
        let mirrored: BalanceRecommendation = rec.into();
        assert_eq!(mirrored.wxhopr, Balance::<WxHOPR>::from(10_800u64));
        assert_eq!(
            mirrored.xdai,
            Balance::<XDai>::from(5_000u64),
            "xdai must mirror the fund amount, not one transaction's fee ceiling"
        );
        assert_eq!(mirrored.channel_stakes, Balance::<WxHOPR>::from(800u64));
        assert_eq!(mirrored.fee_to_start, Balance::<WxHOPR>::from(10_000u64));
        assert_eq!(mirrored.txs_to_start, 3);
        assert_eq!(mirrored.xdai_fee_per_tx, Balance::<XDai>::from(100u64));
    }

    #[test]
    fn wxhopr_deficit_none_when_traffic_good() {
        let allocations = allocs(None, capacity(1_000, 5 * GB), Capacity::default());
        let status = to_funding_status(ideal(100, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, None);
    }

    #[test]
    fn wxhopr_deficit_reported_when_traffic_not_good() {
        let allocations = allocs(None, capacity(30, 0), Capacity::default());
        let status = to_funding_status(ideal(100, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Empty);
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(70u64)));
    }

    #[test]
    fn xdai_deficit_none_when_gas_good() {
        let status = to_funding_status(
            ideal(0, 100),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64),
        );
        assert_eq!(status.gas, FundingLevel::Good);
        assert_eq!(status.xdai_deficit, None);
    }

    #[test]
    fn xdai_deficit_reported_when_gas_not_good() {
        let status = to_funding_status(
            ideal(0, 1_000_000_000_000_000_000_u64), // 1 xDAI ideal
            &CapacityAllocations::default(),
            Balance::<XDai>::from(1_000_000_000_000_000_u64), // 0.001 xDAI on hand
        );
        assert_eq!(status.gas, FundingLevel::Empty);
        assert_eq!(
            status.xdai_deficit,
            Some(Balance::<XDai>::from(999_000_000_000_000_000_u64))
        );
    }

    #[test]
    fn xdai_deficit_reported_even_when_ideal_is_below_low_threshold() {
        let node_xdai = Balance::<XDai>::from(2_000_000_000_000_000_u64); // 0.002 xDAI, in the Low band
        let status = to_funding_status(ideal(0, 0), &CapacityAllocations::default(), node_xdai);
        assert_eq!(status.gas, FundingLevel::Low);
        assert_eq!(
            status.xdai_deficit,
            Some(Balance::<XDai>::from(3_500_000_000_000_000_u64) - node_xdai),
            "deficit must be floored at the Low threshold, not the (lower) ideal recommendation"
        );
    }

    #[test]
    fn good_traffic_and_gas_when_well_funded() {
        // 2 GB each on the channel, Safe, and node EOA = 6 GB pooled, above the 5 GB threshold.
        let allocations = allocs(Some(capacity(100, 2 * GB)), capacity(0, 2 * GB), capacity(100, 2 * GB));
        let status = to_funding_status(
            ideal(100, 100),
            &allocations,
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI — at the Good threshold
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.gas, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, None);
        assert_eq!(status.xdai_deficit, None);
    }

    // `Balance::<WxHOPR>::from(n)` takes wei (10^-18 token). The scientific
    // threshold is 1e-3 *tokens* = 1_000_000_000_000_000 wei, and the cutoff is
    // strict (`< threshold`), so a balance exactly at the threshold is legible
    // in decimal and returns `None`.
    const SCI_THRESHOLD_WEI: u64 = 1_000_000_000_000_000;

    #[test]
    fn wxhopr_scientific_zero_is_none() {
        assert_eq!(wxhopr_scientific(Balance::<WxHOPR>::zero()), None);
    }

    #[test]
    fn wxhopr_scientific_tiny_nonzero_is_formatted() {
        // smallest possible non-zero balance: 1 wei = 1e-18 token
        assert_eq!(
            wxhopr_scientific(Balance::<WxHOPR>::from(1u64)),
            Some("1e-18".to_string())
        );
    }

    #[test]
    fn wxhopr_scientific_below_threshold_is_formatted() {
        // 1e-4 token, well under the 1e-3 cutoff
        assert_eq!(
            wxhopr_scientific(Balance::<WxHOPR>::from(100_000_000_000_000u64)),
            Some("1e-4".to_string())
        );
    }

    #[test]
    fn wxhopr_scientific_keeps_significant_decimals() {
        // 1.5e-4 token -> "1.50e-4" rounded, trimmed to "1.5e-4"
        assert_eq!(
            wxhopr_scientific(Balance::<WxHOPR>::from(150_000_000_000_000u64)),
            Some("1.5e-4".to_string())
        );
    }

    #[test]
    fn wxhopr_scientific_just_below_threshold_is_some() {
        assert!(wxhopr_scientific(Balance::<WxHOPR>::from(SCI_THRESHOLD_WEI - 1)).is_some());
    }

    #[test]
    fn wxhopr_scientific_at_threshold_is_none() {
        // exactly 1e-3 token — decimal form is legible, so no scientific string
        assert_eq!(wxhopr_scientific(Balance::<WxHOPR>::from(SCI_THRESHOLD_WEI)), None);
    }

    #[test]
    fn wxhopr_scientific_above_threshold_is_none() {
        assert_eq!(wxhopr_scientific(Balance::<WxHOPR>::from(SCI_THRESHOLD_WEI + 1)), None);
    }

    // ---- CapacityReconciler ----
    // Fixtures keep byte_capacity = 10 × stake so linear scaling is easy to assert.

    /// stake in wei with bytes pinned at 10 × stake.
    fn cap10(stake: u64) -> Capacity {
        capacity(stake, stake * 10)
    }

    fn total_stake(caps: &CapacityAllocations) -> Balance<WxHOPR> {
        SnapshotTotals::of(caps).stake
    }

    fn total_bytes(caps: &CapacityAllocations) -> u64 {
        SnapshotTotals::of(caps).byte_capacity
    }

    #[test]
    fn reconcile_first_snapshot_passes_through() {
        let mut r = CapacityReconciler::default();
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_holds_total_while_channel_funding_is_unindexed() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // safe halved, no channel visible yet: the missing 100 is in flight
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(200u64));
        assert_eq!(total_bytes(&out), 2000);
        // channel indexed: raw is whole again, no fold-in remains
        let out = r.reconcile(allocs(Some(cap10(100)), Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn reconcile_recovered_safe_flake_does_not_double_count() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // safe lookup flaked to zero for one poll
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(0)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // flake recovered: fold-in must vanish, not stack on top
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_tracks_overlapping_fundings() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // first 100 sent, unindexed
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // second 100 sent while the first arrives: raw total still 100
        let out = r.reconcile(allocs(Some(cap10(100)), Capacity::default(), cap10(0)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // both indexed
        let second = (Address::from([2u8; 20]), cap10(100));
        let mut raw = allocs(Some(cap10(100)), Capacity::default(), cap10(0));
        raw.peer_allocations.insert(second.0, second.1);
        let out = r.reconcile(raw);
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::zero());
    }

    #[test]
    fn reconcile_channel_drainage_passes_through_immediately() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(Some(cap10(200)), Capacity::default(), Capacity::default()));
        // tickets spent: channel stake shrinks — that is real usage, not in-flight funds
        let out = r.reconcile(allocs(Some(cap10(150)), Capacity::default(), Capacity::default()));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(150u64));
    }

    #[test]
    fn reconcile_pending_expires_into_the_lower_total() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        let dropped = allocs(None, Capacity::default(), cap10(100));
        // the drop is masked while polls_left counts down from PENDING_ALLOCATION_MAX_POLLS...
        for _ in 0..PENDING_ALLOCATION_MAX_POLLS {
            let out = r.reconcile(dropped.clone());
            assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        }
        // ...then accepted as a real spend
        let out = r.reconcile(dropped);
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn reconcile_scales_folded_capacity_from_current_ratio() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        // fixtures pin bytes at 10 × stake, so the folded-in 100 must carry 1000 bytes
        assert_eq!(out.safe.byte_capacity, 2000);
    }

    #[test]
    fn reconcile_never_dips_on_transient_overcount() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, cap10(100), cap10(100)));
        // EOA→Safe sweep landed between the two balance reads: briefly counted twice
        let out = r.reconcile(allocs(None, cap10(100), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(300u64));
        // correction back to 200 is masked until expiry (documented trade-off), never below 200
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(300u64));
    }
}
