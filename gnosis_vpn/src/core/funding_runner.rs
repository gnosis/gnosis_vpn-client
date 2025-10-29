use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use serde_json::json;
use thiserror::Error;
use url::Url;

use gnosis_vpn_lib::remote_data;

use std::time::Duration;

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct FundingRunner {
    hopr_params: HoprParams,
    secret_key: String,
}

#[derive(Clone, Debug)]
pub enum Status {
    NotStarted,
    InProgress,
    Success,
    Failed,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
}

impl FundingRunner {
    pub fn new(hopr_params: HoprParams, secret_key: String) -> Self {
        Self {
            hopr_params,
            secret_key,
        }
    }

    pub async fn start(&self) -> Result<bool, Error> {
        let url = Url::parse("https://webapi.hoprnet.org/api/cfp-funding-tool/airdrop")?;
        let keys = self.hopr_params.calc_keys()?;
        let node_address = keys.chain_key.public().to_address();
        let code = self.secret_key.to_string();
        post_funding_tool(url, node_address, code).await
    }
}

async fn post_funding_tool(url: Url, address: Address, code: String) -> Result<bool, Error> {
    retry(ExponentialBackoff::default(), || async {
        let client = reqwest::Client::new();
        let headers = remote_data::json_headers();
        let body = json!({ "address": address.to_string(), "code": code, });

        tracing::debug!(%url, ?headers, %body, "Posting funding tool");

        let url = url.clone();
        let res = client
            .post(url)
            .json(&body)
            .timeout(Duration::from_secs(5 * 60)) // 5 minutes
            .headers(headers)
            .send()
            .await;

        let resp = res
            .map_err(|err| {
                tracing::error!(?err, "Funding tool connect request failed");
                err
            })
            .map_err(Error::from)?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|err| {
                tracing::error!(?err, "Funding tool read response failed");
                err
            })
            .map_err(Error::from)?;

        tracing::debug!(%status, ?text, "Funding tool response");
        Ok(status.is_success())
    })
    .await
}
