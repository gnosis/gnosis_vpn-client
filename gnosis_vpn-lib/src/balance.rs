use reqwest::blocking;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::address::Address;
use crate::entry_node::EntryNode;
use crate::remote_data;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balance {
    pub node_xdai: f64,
    pub safe_wxhopr: f64,
    pub channels_out_wxhopr: f64,
}

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingIssue {
    Unfunded,           // cannot work at all - initial state
    ChannelsOutOfFunds, // does not work - no traffic possible
    SafeOutOfFunds,     // keeps working - cannot top up channels
    SafeLowOnFunds,     // warning before SafeOutOfFunds
    NodeUnderfunded,    // keeps working - cannot open new channels
    NodeLowOnFunds,     // warning before NodeUnderfunded
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("RemoteData error: {0}")]
    RemoteData(#[from] remote_data::Error),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Error parsing float: {0}")]
    ParseFloat(#[from] std::num::ParseFloatError),
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseBalances {
    safe_native: String,
    native: String,
    safe_hopr: String,
    hopr: String,
    safe_hopr_allowance: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChannelEntry {
    id: String,
    peer_address: Address,
    status: String,
    balance: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ChannelStatus {
    /// The channel is closed.
    Closed,
    /// The channel is opened.
    Open,
    /// The channel is pending to be closed.
    /// The timestamp marks the *earliest* possible time when the channel can transition into the `Closed` state.
    PendingToClose(SystemTime),
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseChannels {
    // we don't care about incoming and all
    // incoming: Vec<ChannelEntry>,
    // all: Vec<ChannelEntryFull>,
    outgoing: Vec<ChannelEntry>,
}

impl Balance {
    pub fn new(node_xdai: f64, safe_wxhopr: f64, channels_out_wxhopr: f64) -> Self {
        Balance {
            node_xdai,
            safe_wxhopr,
            channels_out_wxhopr,
        }
    }

    pub fn calc_for_node(client: &blocking::Client, entry_node: &EntryNode) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(entry_node.api_token.as_str())?;
        let bal_path = format!("api/{}/account/balances", entry_node.api_version);
        let bal_url = entry_node.endpoint.join(&bal_path)?;

        tracing::debug!(?headers, %bal_url, "get balances");

        let resp_balances = client
            .get(bal_url)
            .headers(headers.clone())
            .timeout(entry_node.http_timeout)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(remote_data::response_errors)?
            .json::<ResponseBalances>()?;

        let chs_path = format!("api/{}/channels", entry_node.api_version);
        let chs_url = entry_node.endpoint.join(&chs_path)?;

        tracing::debug!(?headers, %chs_url, "get channels");

        let resp_channels = client
            .get(chs_url)
            .headers(headers)
            .timeout(entry_node.http_timeout)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(remote_data::response_errors)?
            .json::<ResponseChannels>()?;

        let channels_out_wxhopr: f64 = resp_channels
            .outgoing
            .iter()
            .filter_map(|ch| ch.balance.split_whitespace().next())
            .filter_map(|bal| bal.parse::<f64>().ok())
            .sum();

        let node_xdai = resp_balances.native.parse::<f64>()?;
        let safe_wxhopr = resp_balances.safe_hopr.parse::<f64>()?;

        Ok(Balance {
            node_xdai,
            safe_wxhopr,
            channels_out_wxhopr,
        })
    }

    pub fn prioritized_funding_issues(&self) -> Vec<FundingIssue> {
        let mut issues = Vec::new();
        if self.node_xdai <= 0.0 && self.safe_wxhopr <= 0.0 {
            issues.push(FundingIssue::Unfunded);
            return issues;
        }
        if self.channels_out_wxhopr < 0.1 {
            issues.push(FundingIssue::ChannelsOutOfFunds);
        }
        if self.safe_wxhopr < 0.1 {
            issues.push(FundingIssue::SafeOutOfFunds);
        } else if self.safe_wxhopr < 1.0 {
            issues.push(FundingIssue::SafeLowOnFunds);
        }
        if self.node_xdai < 0.01 {
            issues.push(FundingIssue::NodeUnderfunded);
        } else if self.node_xdai < 0.1 {
            issues.push(FundingIssue::NodeLowOnFunds);
        }
        issues
    }
}

impl Display for Balance {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balance(node_xdai: {:.6}, safe_wxhopr: {:.6}, channels_out_wxhopr: {:.6})",
            self.node_xdai, self.safe_wxhopr, self.channels_out_wxhopr
        )
    }
}
