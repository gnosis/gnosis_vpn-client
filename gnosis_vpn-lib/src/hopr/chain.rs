use edgli::hopr_lib::Address;
use reqwest::blocking;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use std::time::Duration;

use crate::remote_data;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

pub struct Chain {
    rpc_provider: Url,
    node_address: Address,
}

impl Chain {
    pub fn new(rpc_provider: Url, node_address: Address) -> Self {
        Chain {
            rpc_provider,
            node_address,
        }
    }

    pub fn node_address_balance(&self, client: &blocking::Client, id: &Uuid) -> Result<(), Error> {
        let headers = remote_data::json_headers();

        let data = vec![
"0x252dba4200000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000e0000000000000000000000000d4fdec44db9d44b8f2b6d529620f9c0c7066a2c10000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000002470a08231000000000000000000000000",
        self.node_address.to_string().trim_start_matches("0x"),
"00000000000000000000000000000000000000000000000000000000000000000000000000000000ca11bde05977b3631167028862be2a173976ca11000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000244d2301cc000000000000000000000000",
        self.node_address.to_string().trim_start_matches("0x"),
"00000000000000000000000000000000000000000000000000000000",
        ].concat();

        let params = json!([{
            "from": null,
            "to":"0xcA11bde05977b3631167028862bE2a173976CA11",
            "data": data,
        }, "latest"]);

        let mut json = json!({
            "method": "eth_call",
            "id": id,
            "params": params,
            "jsonrpc": "2.0",
        });

        tracing::debug!(?headers, body = ?json, %self.rpc_provider, "post node address balance request");
        let resp = client
            .post(self.rpc_provider.clone())
            .json(&json)
            .timeout(Duration::from_secs(10))
            .headers(headers)
            .send()?;
        // connection error checks happen before response
        // .map_err(remote_data::connect_errors)?
        // .error_for_status()
        // response error can only be mapped after sending
        // .map_err(open_response_errors)?
        //.json::<Self>()?;

        let bytes = resp.bytes()?;
        let text = std::str::from_utf8(&bytes)?;

        tracing::debug!(%self.rpc_provider, ?text, "node address balance response");
        Ok(())
    }
}
