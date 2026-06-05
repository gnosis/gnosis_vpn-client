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
            FundingIssue::NodeUnderfunded => "node underfunded - cannot open new connections or keep existing ones",
            FundingIssue::NodeLowOnFunds => {
                "node low on funds - will soon be unable to open new connections or keep existing ones"
            }
        };
        write!(f, "{s}")
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
    pub fn to_funding_issues(&self, ticket_value: Balance<WxHOPR>) -> Vec<FundingIssue> {
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
        let issues = balances.to_funding_issues(Balance::<WxHOPR>::from(5u64));

        assert!(issues.contains(&FundingIssue::Unfunded));
        Ok(())
    }

    #[test]
    fn funding_amount_adds_one_ticket_above_threshold() -> anyhow::Result<()> {
        let ticket = Balance::<WxHOPR>::from(10u64);

        assert_eq!(funding_amount(ticket), min_stake_threshold(ticket) + ticket);
        Ok(())
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
