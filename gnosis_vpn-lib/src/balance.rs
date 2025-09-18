use std::fmt::{self, Display};

use edgli::hopr_lib::{WxHOPR, XDai};
use serde::{Deserialize, Serialize};

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
    pub node_xdai: edgli::hopr_lib::Balance<XDai>,
    pub safe_wxhopr: edgli::hopr_lib::Balance<WxHOPR>,
    pub channels_out_wxhopr: edgli::hopr_lib::Balance<WxHOPR>,
}

impl Display for Balances {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balances(node_xdai: {:.6}, safe_wxhopr: {:.6}, channels_out_wxhopr: {:.6})",
            self.node_xdai, self.safe_wxhopr, self.channels_out_wxhopr
        )
    }
}

impl Balances {
    pub async fn load_from(node: &edgli::hopr_lib::Hopr) -> Result<Self, edgli::hopr_lib::errors::HoprLibError> {
        Ok(Balances {
            node_xdai: node.get_balance().await?,
            safe_wxhopr: node.get_safe_balance().await?,
            channels_out_wxhopr: node
                .channels_from(&node.me_onchain())
                .await?
                .into_iter()
                .filter_map(|ch| {
                    if matches!(ch.status, edgli::hopr_lib::ChannelStatus::Open)
                        || matches!(ch.status, edgli::hopr_lib::ChannelStatus::PendingToClose(_))
                    {
                        Some(ch.balance)
                    } else {
                        None
                    }
                })
                .reduce(|acc, x| acc + x)
                .unwrap_or(edgli::hopr_lib::Balance::<WxHOPR>::zero()),
        })
    }
}

impl From<&Balances> for Vec<FundingIssue> {
    fn from(balance: &Balances) -> Self {
        let mut issues = Vec::new();

        if balance.node_xdai <= "0.0 xDai".into() && balance.safe_wxhopr <= "0.0 wxHOPR".into() {
            issues.push(FundingIssue::Unfunded);
            return issues;
        }

        if balance.channels_out_wxhopr < "0.1 wxHOPR".into() {
            issues.push(FundingIssue::ChannelsOutOfFunds);
        }

        if balance.safe_wxhopr < "0.1 wxHOPR".into() {
            issues.push(FundingIssue::SafeOutOfFunds);
        } else if balance.safe_wxhopr < "1.0 wxHOPR".into() {
            issues.push(FundingIssue::SafeLowOnFunds);
        }

        if balance.node_xdai < "0.01 wxHOPR".into() {
            issues.push(FundingIssue::NodeUnderfunded);
        } else if balance.node_xdai < "0.1 wxHOPR".into() {
            issues.push(FundingIssue::NodeLowOnFunds);
        }

        issues
    }
}
