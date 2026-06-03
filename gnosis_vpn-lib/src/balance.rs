pub use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use crate::serde_utils;

use std::collections::HashMap;
use std::fmt::{self, Display};

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

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingIssue {
    Unfunded,           // node xdai zero and no funds in safe or channels - initial state
    ChannelsOutOfFunds, // less than 1 message available in all channels combined
    SafeOutOfFunds,     // less than 1 message available in safe
    SafeLowOnFunds,     // less than 0.5 of ideal safe balance
    NodeUnderfunded,    // xDai is below 100 Gwei - unlikely to cover gas for a transaction
    NodeLowOnFunds,     // xDai is below the ideal amount
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingTool {
    NotStarted,
    InProgress,
    CompletedSuccess,
    CompletedError(String),
}

impl Display for FundingIssue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            FundingIssue::Unfunded => "unfunded - nothing will work",
            FundingIssue::ChannelsOutOfFunds => "channels are out of funds - connections will not work",
            FundingIssue::SafeOutOfFunds => "safe is out of funds - connections will stop working",
            FundingIssue::SafeLowOnFunds => "safe is low on funds - connections will soon stop working",
            FundingIssue::NodeUnderfunded => "node underfunded - cannot open new connections or keep existing ones",
            FundingIssue::NodeLowOnFunds => "node low on funds - will soon be unable to open new connections or keep existing ones",
        };
        write!(f, "{s}")
    }
}

/// Which entity holds a wxHOPR stake: either an open outgoing channel to a peer,
/// or the unallocated balance in the Safe contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", content = "address", rename_all = "snake_case")]
pub enum CapacityAllocator {
    Safe,
    Peer(#[serde(with = "serde_utils::address")] Address),
}

impl From<edgli::strategy::CapacityAllocator> for CapacityAllocator {
    fn from(a: edgli::strategy::CapacityAllocator) -> Self {
        match a {
            edgli::strategy::CapacityAllocator::Peer(addr) => CapacityAllocator::Peer(addr),
            edgli::strategy::CapacityAllocator::Safe => CapacityAllocator::Safe,
        }
    }
}

impl Display for CapacityAllocator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CapacityAllocator::Peer(addr) => write!(f, "channel({})", addr.to_checksum()),
            CapacityAllocator::Safe => write!(f, "safe"),
        }
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

/// A single capacity entry pairing an allocator with its capacity.
/// Used in status responses instead of a HashMap so JSON keys remain strings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapacityEntry {
    pub allocator: CapacityAllocator,
    pub capacity: Capacity,
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

/// Minimum recommended wxHOPR and xDAI balance to open the target number of channels.
/// Computed once during onboarding and surfaced in the PreparingSafe run mode.
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct BalanceRecommendation {
    #[serde(with = "serde_utils::balance")]
    pub wxhopr: Balance<WxHOPR>,
    #[serde(with = "serde_utils::balance")]
    pub xdai: Balance<XDai>,
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

pub fn to_funding_issues(
    ideal: BalanceRecommendation,
    capacity_allocations: &HashMap<CapacityAllocator, Capacity>,
    node_xdai: Balance<XDai>,
) -> Vec<FundingIssue> {
    let mut issues = Vec::new();

    let total_stake = capacity_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>();
    if node_xdai.is_zero() && total_stake.is_zero() {
        issues.push(FundingIssue::Unfunded);
        return issues;
    }

    let channel_messages: u64 = capacity_allocations
        .iter()
        .filter_map(|(k, v)| matches!(k, CapacityAllocator::Peer(_)).then_some(v.min_guaranteed_messages))
        .sum();
    if channel_messages < 1 {
        issues.push(FundingIssue::ChannelsOutOfFunds);
    }

    let safe = capacity_allocations.get(&CapacityAllocator::Safe);
    let safe_messages = safe.map(|c| c.min_guaranteed_messages).unwrap_or(0);
    if safe_messages < 1 {
        issues.push(FundingIssue::SafeOutOfFunds);
    } else {
        let safe_stake = safe.map(|c| c.stake).unwrap_or_default();
        if safe_stake * 2 < ideal.wxhopr {
            issues.push(FundingIssue::SafeLowOnFunds);
        }
    }

    // 100 Gwei — heuristic threshold below which the node is unlikely to cover the gas cost of a typical transaction
    let node_xdai_min_gas_threshold = Balance::<XDai>::from(100_000_000_000u64);
    if node_xdai < node_xdai_min_gas_threshold {
        issues.push(FundingIssue::NodeUnderfunded);
    } else if node_xdai < ideal.xdai {
        issues.push(FundingIssue::NodeLowOnFunds);
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ideal(wxhopr: u64, xdai: u64) -> BalanceRecommendation {
        BalanceRecommendation {
            wxhopr: Balance::<WxHOPR>::from(wxhopr),
            xdai: Balance::<XDai>::from(xdai),
        }
    }

    fn peer_capacity(stake: u64, msgs: u64) -> Capacity {
        Capacity {
            stake: Balance::<WxHOPR>::from(stake),
            expected_messages: msgs,
            min_guaranteed_messages: msgs,
            byte_capacity: 0,
        }
    }

    fn safe_capacity(stake: u64, msgs: u64) -> Capacity {
        Capacity {
            stake: Balance::<WxHOPR>::from(stake),
            expected_messages: msgs,
            min_guaranteed_messages: msgs,
            byte_capacity: 0,
        }
    }

    #[test]
    fn unfunded_when_xdai_and_stake_are_zero() {
        let issues = to_funding_issues(ideal(100, 100), &HashMap::new(), Balance::<XDai>::zero());
        assert_eq!(issues, vec![FundingIssue::Unfunded]);
    }

    #[test]
    fn channels_out_of_funds_when_no_peer_messages() {
        let mut allocs = HashMap::new();
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 5));
        let issues = to_funding_issues(
            ideal(100, 100),
            &allocs,
            Balance::<XDai>::from(1_000_000_000_000_000_u64),
        );
        assert!(issues.contains(&FundingIssue::ChannelsOutOfFunds));
    }

    #[test]
    fn safe_out_of_funds_when_safe_has_no_messages() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 0));
        let issues = to_funding_issues(
            ideal(100, 100),
            &allocs,
            Balance::<XDai>::from(1_000_000_000_000_000_u64),
        );
        assert!(issues.contains(&FundingIssue::SafeOutOfFunds));
    }

    #[test]
    fn safe_low_on_funds_when_stake_below_half_ideal() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        // safe stake 30, ideal wxhopr 100 → 30*2=60 < 100 → SafeLowOnFunds
        allocs.insert(CapacityAllocator::Safe, safe_capacity(30, 5));
        let issues = to_funding_issues(
            ideal(100, 100),
            &allocs,
            Balance::<XDai>::from(1_000_000_000_000_000_u64),
        );
        assert!(issues.contains(&FundingIssue::SafeLowOnFunds));
    }

    #[test]
    fn node_underfunded_when_xdai_below_100_gwei() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 5));
        let issues = to_funding_issues(
            ideal(100, 1_000_000_000_000_u64), // ideal xdai = 1000 Gwei
            &allocs,
            Balance::<XDai>::from(50_000_000_000_u64), // 50 Gwei < 100 Gwei threshold
        );
        assert!(issues.contains(&FundingIssue::NodeUnderfunded));
        assert!(!issues.contains(&FundingIssue::NodeLowOnFunds));
    }

    #[test]
    fn node_low_on_funds_when_xdai_below_ideal() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 5));
        let issues = to_funding_issues(
            ideal(100, 1_000_000_000_000_u64), // ideal xdai = 1000 Gwei
            &allocs,
            Balance::<XDai>::from(500_000_000_000_u64), // 500 Gwei: above threshold, below ideal
        );
        assert!(issues.contains(&FundingIssue::NodeLowOnFunds));
        assert!(!issues.contains(&FundingIssue::NodeUnderfunded));
    }

    #[test]
    fn no_issues_when_well_funded() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 5));
        let issues = to_funding_issues(
            ideal(100, 100),
            &allocs,
            Balance::<XDai>::from(2_000_000_000_000_000_u64),
        );
        assert!(issues.is_empty());
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
