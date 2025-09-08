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
pub struct Info {
    pub node_address: Address,
    pub safe_address: Address,
    pub network: String,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("RemoteData error: {0}")]
    RemoteData(#[from] remote_data::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseAddresses = {
  native: Address,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseInfo = {
  //  skip not interested fields
  //  "announcedAddress",
  //  "listeningAddress",
  //  "chain",
  //  "provider",
  //  "hoprToken",
  //  "hoprChannels",
  //  "hoprNetworkRegistry",
  //  "hoprNodeSafeRegistry",
  //  "hoprManagementModule",
  //  "isEligible",
  //  "connectivityStatus",
  //  "channelClosurePeriod",
  //  "indexerBlock",
  //  "indexerLastLogBlock",
  //  "indexerLastLogChecksum",
  //  "isIndexerCorrupted",
  "network": String,
  "hopr_node_safe": Address,
}

impl Info {
    pub fn new(node_address: Address, safe_address: Address) -> Self {
        Info {
            node_address,
            safe_address,
        }
    }

    pub fn gather(client: &blocking::Client, entry_node: &EntryNode) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(entry_node.api_token.as_str())?;
        let addr_path = format!("api/{}/account/addresses", entry_node.api_version);
        let addr_url = entry_node.endpoint.join(&addr_path)?;

        tracing::debug!(?headers, %url, "get addresses");

        let resp_addresses = client
            .get(bal_url)
            .headers(headers)
            .timeout(entry_node.http_timeout)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(remote_data::response_errors)?
            .json::<ResponseAddresses>()?;

        let info_path = format!("api/{}/node/info", entry_node.api_version);
        let info_url = entry_node.endpoint.join(&info_path)?;

        tracing::debug!(?headers, %url, "get info");

        let resp_info = client
            .get(info_url)
            .headers(headers)
            .timeout(entry_node.http_timeout)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(remote_data::response_errors)?
            .json::<ResponseInfo>()?;

        Ok(Info {
            node_address: resp_addresses.native,
            safe_address: resp_info.hopr_node_safe,
            network: resp_info.network,
        })
    }
}
