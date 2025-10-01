use edgli::hopr_lib::{Balance, GeneralError, WxHOPR, XDai};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingIssue {
    Unfunded,           // cannot work at all - initial state
    ChannelsOutOfFunds, // does not work - no traffic possible
    SafeOutOfFunds,     // keeps working - cannot top up channels
    SafeLowOnFunds,     // warning before SafeOutOfFunds
    NodeUnderfunded,    // keeps working until channels are drained - cannot open new or top up existing channels
    NodeLowOnFunds,     // warning before NodeUnderfunded
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Parsing issue: {0}")]
    Parsing(#[from] GeneralError),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balances {
    pub node_xdai: Balance<XDai>,
    pub safe_wxhopr: Balance<WxHOPR>,
    pub channels_out_wxhopr: Balance<WxHOPR>,
}

impl Display for Balances {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balances(node_xdai: {}, safe_wxhopr: {}, channels_out_wxhopr: {})",
            self.node_xdai, self.safe_wxhopr, self.channels_out_wxhopr
        )
    }
}

impl Balances {
    pub fn to_funding_issues(&self) -> Result<Vec<FundingIssue>, Error> {
        let mut issues = Vec::new();

        if self.node_xdai.is_zero() && self.safe_wxhopr.is_zero() {
            issues.push(FundingIssue::Unfunded);
            return Ok(issues);
        }

        if self.channels_out_wxhopr < "0.1 wxHOPR".parse::<Balance<WxHOPR>>()? {
            issues.push(FundingIssue::ChannelsOutOfFunds);
        }

        if self.safe_wxhopr < "0.1 wxHOPR".parse::<Balance<WxHOPR>>()? {
            issues.push(FundingIssue::SafeOutOfFunds);
        } else if self.safe_wxhopr < "1.0 wxHOPR".parse::<Balance<WxHOPR>>()? {
            issues.push(FundingIssue::SafeLowOnFunds);
        }

        if self.node_xdai < "0.01 xDai".parse::<Balance<XDai>>()? {
            issues.push(FundingIssue::NodeUnderfunded);
        } else if self.node_xdai < "0.1 xDai".parse::<Balance<XDai>>()? {
            issues.push(FundingIssue::NodeLowOnFunds);
        }

        Ok(issues)
    }
}
