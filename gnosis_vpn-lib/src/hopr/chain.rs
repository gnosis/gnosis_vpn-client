use edgli::hopr_lib::Address;
use edgli::hopr_lib::{Balance, WxHOPR, XDai};
use primitive_types::U256;
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
    #[error("JSON RPC id mismatch")]
    JsonRPCIdMismatch,
    #[error("Hex decode error: {0}")]
    HexDecode(#[from] hex::FromHexError),
    #[error("Invalid hex string")]
    InvalidHexString,
}

#[derive(Clone, Debug)]
pub struct Chain {
    rpc_provider: Url,
    node_address: Address,
}

#[derive(Clone, Debug)]
pub struct AccountBalance {
    pub node_xdai: Balance<XDai>,
    pub safe_wxhopr: Balance<WxHOPR>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BalanceResponse {
    jsonrpc: String,
    result: String,
    id: String,
}

impl AccountBalance {
    pub fn new(node_xdai: Balance<XDai>, safe_wxhopr: Balance<WxHOPR>) -> Self {
        AccountBalance { node_xdai, safe_wxhopr }
    }
}

impl Chain {
    pub fn new(rpc_provider: Url, node_address: Address) -> Self {
        Chain {
            rpc_provider,
            node_address,
        }
    }

    pub fn account_balance(&self, client: &blocking::Client) -> Result<AccountBalance, Error> {
        let headers = remote_data::json_headers();

        let data =[
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

        let json = json!({
            "method": "eth_call",
            "id": Uuid::new_v4(),
            "params": params,
            "jsonrpc": "2.0",
        });

        tracing::debug!(?headers, body = ?json, %self.rpc_provider, "post node address balance request");
        let resp = client
            .post(self.rpc_provider.clone())
            .json(&json)
            .timeout(Duration::from_secs(10))
            .headers(headers)
            .send()?
            .json::<BalanceResponse>()?;

        if resp.id != json["id"] {
            return Err(Error::JsonRPCIdMismatch);
        }
        let bytes = hex::decode(resp.result.strip_prefix("0x").ok_or(Error::InvalidHexString)?)?;

        let len = bytes.len();
        let last = &bytes[len - 32..len];
        let third_last = &bytes[len - 32 * 3..len - 32 * 2];

        let last_u256 = U256::from_big_endian(last);
        let third_last_u256 = U256::from_big_endian(third_last);

        let xdai = Balance::<XDai>::from(last_u256);
        let wxhopr = Balance::<WxHOPR>::from(third_last_u256);

        Ok(AccountBalance::new(xdai, wxhopr))
    }
}
