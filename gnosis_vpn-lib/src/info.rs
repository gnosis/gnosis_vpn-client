use reqwest::blocking;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};

use crate::address::Address;
use crate::entry_node::EntryNode;
use crate::remote_data;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    pub node_address: Address,
    pub safe_address: Address,
    pub network: String,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("RemoteData error: {0}")]
    RemoteData(#[from] remote_data::Error),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseAddresses {
    native: Address,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseInfo {
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
    network: String,
    hopr_node_safe: Address,
}

impl Info {
    pub fn new(node_address: Address, safe_address: Address, network: String) -> Self {
        Info {
            network,
            node_address,
            safe_address,
        }
    }

    pub fn gather(client: &blocking::Client, entry_node: &EntryNode) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(entry_node.api_token.as_str())?;
        let addr_path = format!("api/{}/account/addresses", entry_node.api_version);
        let addr_url = entry_node.endpoint.join(&addr_path)?;

        tracing::debug!(?headers, %addr_url, "get addresses");

        let resp_addresses = client
            .get(addr_url)
            .headers(headers.clone())
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

        tracing::debug!(?headers, %info_url, "get info");

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

impl Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Info(node_address: {}, safe_address: {}, network: {})",
            self.node_address, self.safe_address, self.network
        )
    }
}
