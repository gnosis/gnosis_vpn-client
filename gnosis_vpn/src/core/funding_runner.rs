use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use gnosis_vpn_lib::hopr::{Hopr, HoprError, config as hopr_config};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct FundingRunner {
    node_address: Address,
    secret_key: String,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
}

impl FundingRunner {
    pub fn new(node_address: Address, secret_key: String) -> Self {
        Self {
            node_address,
            secret_key,
        }
    }

    pub async fn start(&self) -> Result<bool, Error> {
        let url = Url::parse("https://webapi.hoprnet.org/api/cfp-funding-tool/airdrop")?;
        let address = node_address.to_string();
        let code = secret_hash.to_string();
        post_funding_tool(url, address, code).await
    }
}

async fn post_funding_tool(url: Url, address: String, code: String) -> Result<bool, Error> {
    retry(ExponentialBackoff::default(), || async {
        let client = reqwest::Client::new();
        let headers = remote_data::json_headers();
        let body = json!({ "address": address, "code": code, });

        tracing::debug!(%url, ?headers, %body, "Posting funding tool");

        let res = client
            .post(url)
            .json(&body)
            .timeout(Duration::from_secs(5 * 60)) // 5 minutes
            .headers(headers)
            .send()
            .await;

        let resp = res.map_err(|err| {
            tracing::error!(?err, "Funding tool connect request failed");
            err
        })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|err| {
            tracing::error!(?err, "Funding tool read response failed");
            err
        })?;

        tracing::debug!(%status, ?text, "Funding tool response");
        Ok(status.is_success())
    })
}
