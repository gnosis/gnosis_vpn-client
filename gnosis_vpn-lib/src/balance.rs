use reqwest::{StatusCode, blocking};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use std::cmp;
use std::fmt::{self, Display};
use std::net::SocketAddr;
use std::time::SystemTime;

use crate::address::Address;
use crate::entry_node::EntryNode;
use crate::remote_data;

#[derive(Debug, Serialize, Deserialize)]
pub struct Balance {
    pub node_xdai: f64,
    pub safe_wxhopr: f64,
    pub channels_out_wxhopr: f64,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("RemoteData error: {0}")]
    RemoteData(#[from] remote_data::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseBalances = {
  safe_native: String,
  native: String,
  safe_hopr: String,
  hopr: String,
  safe_hopr_allowance: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChannelEntry = {
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
struct ResponseChannels = {
    // we don't care about incoming and all
    // incoming: Vec<ChannelEntry>,
    // all: Vec<ChannelEntryFull>,
    outgoing: Vec<ChannelEntry>,
}


impl Balance {
    pub fn new(node: String, safe: String, channels_out: String) -> Self {
        Balance {
            node,
            safe,
            channels_out,
        }
    }

    pub fn calc_for_node(client: &blocking::Client, entry_node: &EntryNode) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(entry_node.api_token.as_str())?;
        let bal_path = format!("api/{}/account/balances", entry_node.api_version);
        let bal_url = entry_node.endpoint.join(&bal_path)?;

        tracing::debug!(?headers, %url, "get balances");

        let resp_balances = client
            .get(bal_url)
            .headers(headers)
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

        tracing::debug!(?headers, %url, "get channels");

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
            .filter_map(|ch| ch.balance.split_white_space().next())
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
}
