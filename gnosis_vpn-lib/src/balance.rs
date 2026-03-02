pub use edgli::hopr_lib::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingIssue {
    Unfunded,           // cannot work at all - initial state
    ChannelsOutOfFunds, // less than 1 ticket (10 wxHOPR)
    SafeOutOfFunds,     // less than 1 ticket (10 wxHOPR) - cannot top up channels
    SafeLowOnFunds,     // lower than min_stake_threshold * 2
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

impl Display for FundingTool {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FundingTool::NotStarted => write!(f, "Funding not started"),
            FundingTool::InProgress => write!(f, "Funding in progress"),
            FundingTool::CompletedSuccess => write!(f, "Funding successful"),
            FundingTool::CompletedError(err) => write!(f, "Funding error: {}", err),
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

impl Balances {
    pub fn to_funding_issues(&self, _channel_targets_len: usize, ticket_value: Balance<WxHOPR>) -> Vec<FundingIssue> {
        let mut issues = Vec::new();

        if self.node_xdai.is_zero() && self.safe_wxhopr.is_zero() {
            issues.push(FundingIssue::Unfunded);
            return issues;
        }

        let all_channel_funds = self.channels_out.values().copied().sum::<Balance<WxHOPR>>();
        if all_channel_funds < min_stake_threshold(ticket_value) {
            issues.push(FundingIssue::ChannelsOutOfFunds);
        }

        if self.safe_wxhopr < funding_amount(ticket_value) {
            issues.push(FundingIssue::SafeOutOfFunds);
        } else if self.safe_wxhopr < (funding_amount(ticket_value) * 2) {
            issues.push(FundingIssue::SafeLowOnFunds);
        }

        if self.node_xdai < min_funds_threshold() {
            issues.push(FundingIssue::NodeUnderfunded);
        } else if self.node_xdai < (min_funds_threshold() * 2) {
            issues.push(FundingIssue::NodeLowOnFunds);
        }

        issues
    }
}

/// worth 1 more ticket than min_stake_threshold
pub fn funding_amount(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
    min_stake_threshold(ticket_value) + ticket_value
}

/// imposed by 3hops. 3 times ticket_value at least are needed in a channel in case the 1st relayer wants to redeem a winning ticket
pub fn min_stake_threshold(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
    ticket_value * 3
}

/// Based on the fixed gas price we use (3gwei) and our average gas/tx consumption (250'000)
pub fn min_funds_threshold() -> Balance<XDai> {
    Balance::<XDai>::from(750000000000000_u64) // 0.00075 xDai = 3 gwei * 250'000 gas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_funding_issues_marks_unfunded_when_all_balances_zero() -> anyhow::Result<()> {
        let balances = Balances {
            node_xdai: Balance::<XDai>::zero(),
            safe_wxhopr: Balance::<WxHOPR>::zero(),
            channels_out: HashMap::new(),
        };
        let issues = balances.to_funding_issues(2, Balance::<WxHOPR>::from(5u64));

        assert!(issues.contains(&FundingIssue::Unfunded));
        Ok(())
    }

    #[test]
    fn funding_amount_adds_one_ticket_above_threshold() -> anyhow::Result<()> {
        let ticket = Balance::<WxHOPR>::from(10u64);

        assert_eq!(funding_amount(ticket), min_stake_threshold(ticket) + ticket);
        Ok(())
    }
}
