use alloy::primitives::U256;
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::{Address, Balance, WxHOPR};
use rand::Rng;
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

use std::time::Duration;

use gnosis_vpn_lib::balance;
use gnosis_vpn_lib::chain::client::GnosisRpcClient;
use gnosis_vpn_lib::chain::contracts::NetworkSpecifications;
use gnosis_vpn_lib::chain::contracts::{SafeModuleDeploymentInputs, SafeModuleDeploymentResult};
use gnosis_vpn_lib::chain::errors::ChainError;
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config};
use gnosis_vpn_lib::remote_data;
use gnosis_vpn_lib::ticket_stats::{self, TicketStats};

use crate::hopr_params::{self, HoprParams};

pub enum Results {
    FundChannel {
        address: Address,
        res: Result<(), hopr_api::ChannelError>,
    },
    PreSafe {
        res: Result<balance::PreSafe, Error>,
    },
    TicketStats {
        res: Result<ticket_stats::TicketStats, Error>,
    },
    SafeDeployment {
        res: Result<SafeModuleDeploymentResult, Error>,
    },
    FundingTool {
        res: Result<bool, Error>,
    },
    Hopr {
        res: Result<Hopr, Error>,
    },
}

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    PreSafe(#[from] balance::Error),
    #[error(transparent)]
    TicketStats(#[from] ticket_stats::Error),
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    HoprConfig(#[from] hopr_config::Error),
    #[error(transparent)]
    Hopr(#[from] HoprError),
}

pub async fn presafe(hopr_params: &HoprParams, results_sender: &mpsc::Sender<Results>) {
    let res = run_presafe(hopr_params).await;
    let _ = results_sender.send(Results::PreSafe { res }).await;
}

pub async fn ticket_stats(hopr_params: &HoprParams, results_sender: &mpsc::Sender<Results>) {
    let res = run_ticket_stats(hopr_params).await;
    let _ = results_sender.send(Results::TicketStats { res }).await;
}

pub async fn safe_deployment(
    hopr_params: &HoprParams,
    presafe: &balance::PreSafe,
    results_sender: &mpsc::Sender<Results>,
) {
    let res = run_safe_deployment(hopr_params, presafe).await;
    let _ = results_sender.send(Results::SafeDeployment { res }).await;
}

pub async fn funding_tool(url: &Url, address: Address, code: &str, results_sender: &mpsc::Sender<Results>) {
    let res = run_funding_tool(url, address, code).await;
    let _ = results_sender.send(Results::FundingTool { res }).await;
}

pub async fn hopr(hopr_params: &HoprParams, ticket_value: Balance<WxHOPR>, results_sender: &mpsc::Sender<Results>) {
    let res = run_hopr(hopr_params, ticket_value).await;
    let _ = results_sender.send(Results::Hopr { res }).await;
}

async fn run_presafe(hopr_params: &HoprParams) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting presafe balance runner");
    let keys = hopr_params.calc_keys()?;
    let private_key = keys.chain_key.clone();
    let rpc_provider = hopr_params.rpc_provider.clone();
    let node_address = keys.chain_key.public().to_address();
    retry(ExponentialBackoff::default(), || async {
        let presafe = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address)
            .await
            .map_err(Error::from)?;
        Ok(presafe)
    })
    .await
}

async fn run_ticket_stats(hopr_params: &HoprParams) -> Result<ticket_stats::TicketStats, Error> {
    tracing::debug!("starting ticket stats runner");
    let keys = hopr_params.calc_keys()?;
    let private_key = keys.chain_key;
    let rpc_provider = hopr_params.rpc_provider.clone();
    let network = hopr_params.network.clone();
    retry(ExponentialBackoff::default(), || async {
        let stats = TicketStats::fetch(
            &private_key,
            rpc_provider.as_str(),
            &NetworkSpecifications::from_network(&network),
        )
        .await
        .map_err(Error::from)?;
        Ok(stats)
    })
    .await
}

async fn run_safe_deployment(
    hopr_params: &HoprParams,
    presafe: &balance::PreSafe,
) -> Result<SafeModuleDeploymentResult, Error> {
    tracing::debug!("starting safe deployment runner");
    let keys = hopr_params.calc_keys()?;
    let private_key = keys.chain_key.clone();
    let rpc_provider = hopr_params.rpc_provider.clone();
    let node_address = keys.chain_key.public().to_address();
    let token_u256 = presafe.node_wxhopr.amount();
    let token_bytes: [u8; 32] = token_u256.to_big_endian();
    let token_amount: U256 = U256::from_be_bytes::<32>(token_bytes);
    let network = hopr_params.network.clone();
    retry(ExponentialBackoff::default(), || async {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let nonce = U256::from_be_bytes(bytes);
        let client = GnosisRpcClient::with_url(private_key, rpc_provider.as_str())
            .await
            .map_err(Error::from)?;
        let safe_module_deployment_inputs =
            SafeModuleDeploymentInputs::new(nonce, token_amount, vec![node_address.into()]);
        let res = safe_module_deployment_inputs
            .deploy(&client.provider, network)
            .await
            .map_err(Error::from)?;
        Ok(res)
    })
    .await
}

async fn run_funding_tool(url: &Url, address: Address, code: &str) -> Result<bool, Error> {
    let client = reqwest::Client::new();
    let headers = remote_data::json_headers();
    let body = json!({ "address": address.to_string(), "code": code, });
    let url = url.clone();
    tracing::debug!(%url, ?headers, %body, "Posting funding tool");
    retry(ExponentialBackoff::default(), || async {
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

async fn run_hopr(hopr_params: &HoprParams, ticket_value: Balance<WxHOPR>) -> Result<Hopr, Error> {
    let cfg = match hopr_params.config_mode.clone() {
        // use user provided configuration path
        hopr_params::ConfigFileMode::Manual(path) => hopr_config::from_path(path.as_ref())?,
        // check status of config generation
        hopr_params::ConfigFileMode::Generated => hopr_config::generate(
            hopr_params.network.clone(),
            hopr_params.rpc_provider.clone(),
            ticket_value,
        )?,
    };
    let keys = hopr_params.calc_keys()?;
    Hopr::new(cfg, keys).await.map_err(Error::from)
}
