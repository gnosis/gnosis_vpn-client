//! Various runner tasks that might get extracted into their own modules once applicable.
//! These function expect to be spawn and will deliver their result or progress via channels.

use alloy::primitives::U256;
use backoff::ExponentialBackoff;
use backoff::future::retry;
use bytesize::ByteSize;
use edgli::hopr_lib::SurbBalancerConfig;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Address, Balance, WxHOPR};
use human_bandwidth::re::bandwidth::Bandwidth;
use rand::Rng;
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use url::Url;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::Duration;

use crate::balance;
use crate::chain::client::GnosisRpcClient;
use crate::chain::contracts::NetworkSpecifications;
use crate::chain::contracts::{SafeModuleDeploymentInputs, SafeModuleDeploymentResult};
use crate::chain::errors::ChainError;
use crate::connection;
use crate::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config};
use crate::hopr_params::{self, HoprParams};
use crate::log_output;
use crate::remote_data;
use crate::ticket_stats::{self, TicketStats};

/// Results indicate events that arise from concurrent runners.
/// These runners are usually spawned and want to report data or progress back to the core application loop.
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
    SafePersisted,
    FundingTool {
        res: Result<bool, Error>,
    },
    Hopr {
        res: Result<Hopr, Error>,
    },
    Balances {
        res: Result<balance::Balances, Error>,
    },
    ConnectedPeers {
        res: Result<Vec<Address>, Error>,
    },
    HoprRunning,
    ConnectionEvent {
        evt: connection::up::runner::Event,
    },
    ConnectionResult {
        res: Result<(), connection::up::runner::Error>,
    },
    DisconnectionEvent {
        wg_public_key: String,
        evt: connection::down::runner::Event,
    },
    DisconnectionResult {
        wg_public_key: String,
        res: Result<(), connection::down::runner::Error>,
    },
}

#[derive(Debug, Error)]
pub enum Error {
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
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    ChannelError(#[from] hopr_api::ChannelError),
}

pub async fn ticket_stats(hopr_params: HoprParams, results_sender: mpsc::Sender<Results>) {
    let res = run_ticket_stats(hopr_params).await;
    let _ = results_sender.send(Results::TicketStats { res }).await;
}

pub async fn presafe(hopr_params: HoprParams, results_sender: mpsc::Sender<Results>) {
    let res = run_presafe(hopr_params).await;
    let _ = results_sender.send(Results::PreSafe { res }).await;
}

pub async fn funding_tool(hopr_params: HoprParams, code: String, results_sender: mpsc::Sender<Results>) {
    let res = run_funding_tool(hopr_params, code).await;
    let _ = results_sender.send(Results::FundingTool { res }).await;
}

pub async fn safe_deployment(
    hopr_params: HoprParams,
    presafe: balance::PreSafe,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_safe_deployment(hopr_params, presafe).await;
    let _ = results_sender.send(Results::SafeDeployment { res }).await;
}

pub async fn persist_safe(safe_module: hopr_config::SafeModule, results_sender: mpsc::Sender<Results>) {
    tracing::debug!("persisting safe module");
    while let Err(err) = hopr_config::store_safe(&safe_module).await {
        log_output::print_safe_module_storage_error(err);
        time::sleep(Duration::from_secs(5)).await;
    }
    let _ = results_sender.send(Results::SafePersisted).await;
}

pub async fn hopr(hopr_params: HoprParams, ticket_value: Balance<WxHOPR>, results_sender: mpsc::Sender<Results>) {
    let res = run_hopr(hopr_params, ticket_value).await;
    let _ = results_sender.send(Results::Hopr { res }).await;
}

pub async fn balances(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    tracing::debug!("starting balances runner");
    let res = hopr.balances().await.map_err(Error::from);
    let _ = results_sender.send(Results::Balances { res }).await;
}

pub async fn wait_for_running(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    while hopr.status() != HoprState::Running {
        time::sleep(Duration::from_secs(1)).await;
    }
    let _ = results_sender.send(Results::HoprRunning).await;
}

pub async fn fund_channel(
    hopr: Arc<Hopr>,
    address: Address,
    ticket_value: Balance<WxHOPR>,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_fund_channel(hopr, address, ticket_value).await;
    let _ = results_sender.send(Results::FundChannel { address, res }).await;
}

pub async fn connected_peers(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    let res = hopr.connected_peers().await.map_err(Error::from);
    let _ = results_sender.send(Results::ConnectedPeers { res }).await;
}

async fn run_presafe(hopr_params: HoprParams) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting presafe balance runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key.clone();
    let rpc_provider = hopr_params.rpc_provider();
    let node_address = keys.chain_key.public().to_address();
    retry(ExponentialBackoff::default(), || async {
        let presafe = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address)
            .await
            .map_err(Error::from)?;
        Ok(presafe)
    })
    .await
}

async fn run_ticket_stats(hopr_params: HoprParams) -> Result<ticket_stats::TicketStats, Error> {
    tracing::debug!("starting ticket stats runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key;
    let rpc_provider = hopr_params.rpc_provider();
    let network = hopr_params.network();
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
    hopr_params: HoprParams,
    presafe: balance::PreSafe,
) -> Result<SafeModuleDeploymentResult, Error> {
    tracing::debug!("starting safe deployment runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key.clone();
    let rpc_provider = hopr_params.rpc_provider();
    let node_address = keys.chain_key.public().to_address();
    let token_u256 = presafe.node_wxhopr.amount();
    let token_bytes: [u8; 32] = token_u256.to_big_endian();
    let token_amount: U256 = U256::from_be_bytes::<32>(token_bytes);
    let network = hopr_params.network();
    retry(ExponentialBackoff::default(), || async {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let nonce = U256::from_be_bytes(bytes);
        let client = GnosisRpcClient::with_url(private_key.clone(), rpc_provider.as_str())
            .await
            .map_err(Error::from)?;
        let safe_module_deployment_inputs =
            SafeModuleDeploymentInputs::new(nonce, token_amount, vec![node_address.into()]);
        let res = safe_module_deployment_inputs
            .deploy(&client.provider, network.clone())
            .await
            .map_err(Error::from)?;
        Ok(res)
    })
    .await
}

async fn run_funding_tool(hopr_params: HoprParams, code: String) -> Result<bool, Error> {
    let keys = hopr_params.calc_keys().await?;
    let node_address = keys.chain_key.public().to_address();
    let url = Url::parse("https://webapi.hoprnet.org/api/cfp-funding-tool/airdrop")?;
    let client = reqwest::Client::new();
    let headers = remote_data::json_headers();
    let body = json!({ "address": node_address.to_string(), "code": code, });
    tracing::debug!(%url, ?headers, %body, "Posting funding tool");
    retry(ExponentialBackoff::default(), || async {
        let res = client
            .post(url.clone())
            .json(&body)
            .timeout(Duration::from_secs(5 * 60)) // 5 minutes
            .headers(headers.clone())
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

async fn run_hopr(hopr_params: HoprParams, ticket_value: Balance<WxHOPR>) -> Result<Hopr, Error> {
    tracing::debug!("starting hopr runner");
    let cfg = hopr_params.to_config(ticket_value).await?;
    let keys = hopr_params.calc_keys().await?;
    Hopr::new(cfg, keys).await.map_err(Error::from)
}

async fn run_fund_channel(
    hopr: Arc<Hopr>,
    address: Address,
    ticket_value: Balance<WxHOPR>,
) -> Result<(), hopr_api::ChannelError> {
    let amount = balance::funding_amount(ticket_value);
    let threshold = balance::min_stake_threshold(ticket_value);
    tracing::debug!(%address, %amount, %threshold, "starting fund channel runner");
    retry(ExponentialBackoff::default(), || async {
        hopr.ensure_channel_open_and_funded(address, amount, threshold).await?;
        Ok(())
    })
    .await
}

impl Display for Results {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Results::FundChannel { address, res } => match res {
                Ok(_) => write!(f, "FundChannel to {}: Success", address),
                Err(err) => write!(f, "FundChannel to {}: Error({})", address, err),
            },
            Results::PreSafe { res } => match res {
                Ok(presafe) => write!(f, "PreSafe: {}", presafe),
                Err(err) => write!(f, "PreSafe: Error({})", err),
            },
            Results::TicketStats { res } => match res {
                Ok(stats) => write!(f, "TicketStats: {}", stats),
                Err(err) => write!(f, "TicketStats: Error({})", err),
            },
            Results::SafeDeployment { res } => match res {
                Ok(deployment) => write!(f, "SafeDeployment: {:?}", deployment),
                Err(err) => write!(f, "SafeDeployment: Error({})", err),
            },
            Results::SafePersisted => write!(f, "SafePersisted: Success"),
            Results::FundingTool { res } => match res {
                Ok(success) => write!(f, "FundingTool: Success({})", success),
                Err(err) => write!(f, "FundingTool: Error({})", err),
            },
            Results::Hopr { res } => match res {
                Ok(_) => write!(f, "Hopr: Initialized Successfully"),
                Err(err) => write!(f, "Hopr: Error({})", err),
            },
            Results::Balances { res } => match res {
                Ok(balances) => write!(f, "Balances: {}", balances),
                Err(err) => write!(f, "Balances: Error({})", err),
            },
            Results::ConnectedPeers { res } => match res {
                Ok(peers) => write!(f, "ConnectedPeers: {:?}", peers),
                Err(err) => write!(f, "ConnectedPeers: Error({})", err),
            },
            Results::HoprRunning => write!(f, "HoprRunning: Node is running"),
            Results::ConnectionEvent { evt } => write!(f, "ConnectionEvent: {}", evt),
            Results::ConnectionResult { res } => match res {
                Ok(_) => write!(f, "ConnectionResult: Success"),
                Err(err) => write!(f, "ConnectionResult: Error({})", err),
            },
            Results::DisconnectionEvent { wg_public_key, evt } => {
                write!(f, "DisconnectionEvent ({}): {}", wg_public_key, evt)
            }
            Results::DisconnectionResult { wg_public_key, res } => match res {
                Ok(_) => write!(f, "DisconnectionResult ({}): Success", wg_public_key),
                Err(err) => write!(f, "DisconnectionResult ({}): Error({})", wg_public_key, err),
            },
        }
    }
}

pub fn to_surb_balancer_config(response_buffer: ByteSize, max_surb_upstream: Bandwidth) -> SurbBalancerConfig {
    // Buffer worth at least 2 reply packets
    if response_buffer.as_u64() >= 2 * edgli::hopr_lib::SESSION_MTU as u64 {
        SurbBalancerConfig {
            target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::SESSION_MTU as u64,
            max_surbs_per_sec: (max_surb_upstream.as_bps() as usize / (8 * edgli::hopr_lib::SURB_SIZE)) as u64,
            ..Default::default()
        }
    } else {
        // Use defaults otherwise
        Default::default()
    }
}
