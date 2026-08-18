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
    /// wxHOPR still needed to reach the ideal recommendation; `None` while `traffic` is `Good`.
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

const TRAFFIC_EMPTY_BELOW_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const TRAFFIC_LOW_BELOW_BYTES: u64 = 5 * 1024 * 1024 * 1024;
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

    let traffic = if total_bytes < TRAFFIC_EMPTY_BELOW_BYTES {
        FundingLevel::Empty
    } else if total_bytes < TRAFFIC_LOW_BELOW_BYTES {
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

    // saturating_sub floors an already-exceeded ideal at zero instead of underflowing.
    let wxhopr_deficit = (traffic != FundingLevel::Good)
        .then(|| ideal.wxhopr - total_stake)
        .filter(|d| !d.is_zero());
    let xdai_deficit = (gas != FundingLevel::Good)
        .then(|| ideal.xdai - node_xdai)
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

    const GB: u64 = 1024 * 1024 * 1024;

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
        // 1 GB each, pooled to 3 GB: at the Empty/Low boundary, not below it.
        let allocations = allocs(Some(capacity(0, GB)), capacity(0, GB), capacity(0, GB));
        let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Low);
    }

    #[test]
    fn traffic_low_between_thresholds() {
        let allocations = allocs(None, capacity(0, 4 * GB), Capacity::default());
        let status = to_funding_status(ideal(0, 0), &allocations, Balance::<XDai>::zero());
        assert_eq!(status.traffic, FundingLevel::Low);
    }

    #[test]
    fn traffic_good_when_eoa_alone_covers_5gb() {
        // unswept EOA wxHOPR alone counts toward traffic.
        let allocations = allocs(None, capacity(0, 5 * GB), Capacity::default());
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
}
