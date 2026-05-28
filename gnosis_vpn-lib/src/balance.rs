pub use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

pub fn human_wxhopr(b: Balance<WxHOPR>) -> String {
    let v: f64 = b.amount_in_base_units().parse().unwrap_or(0.0);
    match v {
        v if v >= 1.0 => format!("{:.1} wxHOPR", v),
        v if v >= 1e-3 => format!("{:.1} MilliwxHOPR", v / 1e-3),
        v if v >= 1e-6 => format!("{:.1} MicrowxHOPR", v / 1e-6),
        v if v >= 1e-9 => format!("{:.1} GwxHopli", v / 1e-9),
        v if v >= 1e-12 => format!("{:.1} MwxHopli", v / 1e-12),
        v if v >= 1e-15 => format!("{:.1} KwxHopli", v / 1e-15),
        _ => format!("{:.0} wxHopli", v * 1e18),
    }
}

use crate::serde_utils;

use std::collections::HashMap;
use std::fmt::{self, Display};

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingIssue {
    Unfunded,           // node xdai zero and no funds in safe or channels - initial state
    ChannelsOutOfFunds, // less than 1 message available in all channels combined
    SafeOutOfFunds,     // less than 1 message available in safe
    SafeLowOnFunds,     // less than 0.5 of ideal safe balance
    NodeUnderfunded,    // lower than 0.0075 xDai
    NodeLowOnFunds,     // lower than 0.0075 xDai * 2
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
            FundingIssue::NodeUnderfunded => "underfunded - cannot open new connection or keep existing ones",
            FundingIssue::NodeLowOnFunds => "low on funds - soon cannot open new connection or keep existing ones",
        };
        write!(f, "{s}")
    }
}

/// Which entity holds a wxHOPR stake: either an open outgoing channel to a peer,
/// or the unallocated balance in the Safe contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum CapacityAllocator {
    Peer(#[serde(with = "serde_utils::address")] Address),
    Safe,
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

/// Based on the fixed gas price we use (3gwei) and our average gas/tx consumption (250'000)
pub fn min_funds_threshold() -> Balance<XDai> {
    Balance::<XDai>::from(750000000000000_u64) // 0.00075 xDai = 3 gwei * 250'000 gas
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
        .filter_map(|(k, v)| matches!(k, CapacityAllocator::Peer(_)).then_some(v.expected_messages))
        .sum();
    if channel_messages < 1 {
        issues.push(FundingIssue::ChannelsOutOfFunds);
    }

    let safe = capacity_allocations.get(&CapacityAllocator::Safe);
    let safe_messages = safe.map(|c| c.expected_messages).unwrap_or(0);
    if safe_messages < 1 {
        issues.push(FundingIssue::SafeOutOfFunds);
    } else {
        let safe_stake = safe.map(|c| c.stake).unwrap_or_default();
        if safe_stake * 2 < ideal.wxhopr {
            issues.push(FundingIssue::SafeLowOnFunds);
        }
    }

    if node_xdai < min_funds_threshold() {
        issues.push(FundingIssue::NodeUnderfunded);
    } else if node_xdai < (min_funds_threshold() * 2) {
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
        Capacity { stake: Balance::<WxHOPR>::from(stake), expected_messages: msgs, byte_capacity: 0 }
    }

    fn safe_capacity(stake: u64, msgs: u64) -> Capacity {
        Capacity { stake: Balance::<WxHOPR>::from(stake), expected_messages: msgs, byte_capacity: 0 }
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
        let issues = to_funding_issues(ideal(100, 100), &allocs, Balance::<XDai>::from(1_000_000_000_000_000_u64));
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
        let issues = to_funding_issues(ideal(100, 100), &allocs, Balance::<XDai>::from(1_000_000_000_000_000_u64));
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
        let issues = to_funding_issues(ideal(100, 100), &allocs, Balance::<XDai>::from(1_000_000_000_000_000_u64));
        assert!(issues.contains(&FundingIssue::SafeLowOnFunds));
    }

    #[test]
    fn no_issues_when_well_funded() {
        let mut allocs = HashMap::new();
        allocs.insert(
            CapacityAllocator::Peer(Address::from([1u8; 20])),
            peer_capacity(100, 10),
        );
        allocs.insert(CapacityAllocator::Safe, safe_capacity(100, 5));
        // xdai well above 0.0015 xDai (2x threshold)
        let issues = to_funding_issues(ideal(100, 100), &allocs, Balance::<XDai>::from(2_000_000_000_000_000_u64));
        assert!(issues.is_empty());
    }
}
