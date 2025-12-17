//! Various runner tasks that might get extracted into their own modules once applicable.
//! These function expect to be spawn and will deliver their result or progress via channels.

use backon::{ExponentialBuilder, Retryable};
use bytesize::ByteSize;
use edgli::hopr_lib::exports::api::chain::{ChainReadSafeOperations, SafeSelector};
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Address, Balance, IntoEndian, WxHOPR, XDai};
use edgli::hopr_lib::{IpProtocol, SurbBalancerConfig};
use edgli::{SafeModuleDeploymentInputs, SafeModuleDeploymentResult};
use human_bandwidth::re::bandwidth::Bandwidth;
use rand::{Rng, random};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use url::Url;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::Duration;

use crate::balance;
use crate::connection;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config};
use crate::hopr_params::{self, HoprParams};
use crate::log_output;
use crate::remote_data;
use crate::ticket_stats::{self, TicketStats};

const SAFE_RETRIEVAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Results indicate events that arise from concurrent runners.
/// These runners are usually spawned and want to report data or progress back to the core application loop.
pub enum Results {
    FundChannel {
        address: Address,
        target_dest: Address,
        res: Result<(), hopr_api::ChannelError>,
    },
    PreSafe {
        res: Result<balance::PreSafe, Error>,
    },
    TicketStats {
        res: Result<TicketStats, Error>,
    },
    SafeDeployment {
        res: Result<SafeModuleDeploymentResult, Error>,
    },
    SafePersisted,
    FundingTool {
        res: Result<Option<String>, Error>,
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
        evt: connection::up::Event,
    },
    ConnectionResultPreWg {
        res: Result<SessionClientMetadata, connection::up::Error>,
    },
    ConnectionResultPostWg {
        res: Result<(), connection::up::Error>,
    },
    DisconnectionEvent {
        wg_public_key: String,
        evt: connection::down::runner::Event,
    },
    DisconnectionResult {
        wg_public_key: String,
        res: Result<(), connection::down::runner::Error>,
    },
    SessionMonitorFailed,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    PreSafe(#[from] balance::Error),
    #[error(transparent)]
    TicketStats(#[from] ticket_stats::Error),
    #[error("chain error: {0}")]
    Chain(String),
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
    #[error("Funding tool error: {0}")]
    FundingTool(String),
}

#[derive(Debug, Deserialize)]
struct UnauthorizedError {
    error: String,
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
    target_dest: Address,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_fund_channel(hopr, address, ticket_value).await;
    let _ = results_sender
        .send(Results::FundChannel {
            address,
            res,
            target_dest,
        })
        .await;
}

pub async fn connected_peers(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    tracing::debug!("starting connected peers runner");
    let res = hopr.connected_peers().await.map_err(Error::from);
    let _ = results_sender.send(Results::ConnectedPeers { res }).await;
}

pub async fn monitor_session(hopr: Arc<Hopr>, session: &SessionClientMetadata, results_sender: mpsc::Sender<Results>) {
    run_monitor_session(hopr, session).await;
    let _ = results_sender.send(Results::SessionMonitorFailed).await;
}

async fn run_presafe(hopr_params: HoprParams) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting presafe balance runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key.clone();
    let node_address = keys.chain_key.public().to_address();
    let url = hopr_params.blokli_url_with_fallback(edgli::blokli::DEFAULT_BLOKLI_URL)?;
    (|| async {
        let (balance_wxhopr, balance_xdai) =
            edgli::blokli::with_safeless_blokli_connector(&private_key, url.clone(), |connector| async move {
                let balance_wxhopr = edgli::hopr_lib::exports::api::chain::ChainValues::balance::<
                    WxHOPR,
                    edgli::hopr_lib::Address,
                >(&connector, node_address)
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;
                let balance_xdai = edgli::hopr_lib::exports::api::chain::ChainValues::balance::<
                    XDai,
                    edgli::hopr_lib::Address,
                >(&connector, node_address)
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;

                Ok::<_, Error>((balance_wxhopr, balance_xdai))
            })
            .await
            .map_err(|e| Error::Chain(e.to_string()))?
            .await?;

        Ok(balance::PreSafe {
            node_xdai: balance_xdai,
            node_wxhopr: balance_wxhopr,
        })
    })
    .retry(ExponentialBuilder::default())
    .await
}

async fn run_ticket_stats(hopr_params: HoprParams) -> Result<TicketStats, Error> {
    tracing::debug!("starting ticket stats runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key;
    let url = hopr_params.blokli_url_with_fallback(edgli::blokli::DEFAULT_BLOKLI_URL)?;
    (|| async {
        let (ticket_price, winning_probability) =
            edgli::blokli::with_safeless_blokli_connector(&private_key, url.clone(), |connector| async move {
                let ticket_price = edgli::hopr_lib::exports::api::chain::ChainValues::minimum_ticket_price(&connector)
                    .await
                    .map_err(|e| Error::Chain(e.to_string()))?;
                let win_prob =
                    edgli::hopr_lib::exports::api::chain::ChainValues::minimum_incoming_ticket_win_prob(&connector)
                        .await
                        .map_err(|e| Error::Chain(e.to_string()))?;

                Ok::<_, Error>((ticket_price, win_prob))
            })
            .await
            .map_err(|e| Error::Chain(e.to_string()))?
            .await?;

        Ok(TicketStats {
            ticket_price,
            winning_probability: winning_probability.as_f64(),
        })
    })
    .retry(ExponentialBuilder::default())
    .await
}

async fn run_safe_deployment(
    hopr_params: HoprParams,
    presafe: balance::PreSafe,
) -> Result<SafeModuleDeploymentResult, Error> {
    tracing::debug!("starting safe deployment runner");
    let keys = hopr_params.calc_keys().await?;
    let private_key = keys.chain_key.clone();
    let node_address = keys.chain_key.public().to_address();
    let token_u256 = presafe.node_wxhopr.amount();
    let token_bytes: [u8; 32] = token_u256.to_big_endian();
    let token_amount = edgli::hopr_lib::U256::from_be_bytes(token_bytes);
    let nonce = edgli::hopr_lib::U256::from(random::<u64>());
    let url = hopr_params.blokli_url_with_fallback(edgli::blokli::DEFAULT_BLOKLI_URL)?;

    (|| async {
        // Deploy safe
        let safe = edgli::blokli::with_safeless_blokli_connector(&private_key, url.clone(), |connector| async move {
            let inputs = SafeModuleDeploymentInputs {
                token_amount,
                nonce,
                admins: vec![node_address],
            };

            let signed_tx = edgli::blokli::safe_creation_payload_generator(&connector, inputs)
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;

            let transaction = edgli::connector::blokli_client::BlokliTransactionClient::submit_transaction(
                connector.client(),
                signed_tx.as_ref(),
            )
            .await;
            tracing::debug!(?transaction, "safe deployment transaction submitted");

            let safe = connector
                .await_safe_deployment(SafeSelector::Owner(node_address), SAFE_RETRIEVAL_TIMEOUT)
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;

            Ok::<_, Error>(safe)
        })
        .await
        .map_err(|e| Error::Chain(e.to_string()))?
        .await?;

        Ok(SafeModuleDeploymentResult {
            safe_address: safe.address,
            module_address: safe.module,
        })
    })
    .retry(ExponentialBuilder::default())
    .await
}

// Posts to the HOPR funding tool API to request an airdrop using the provided code.
// Returns final errors in ok branch to break exponential backoff retries.
async fn run_funding_tool(hopr_params: HoprParams, code: String) -> Result<Option<String>, Error> {
    let keys = hopr_params.calc_keys().await?;
    let node_address = keys.chain_key.public().to_address();
    let url = Url::parse("https://webapi.hoprnet.org/api/cfp-funding-tool/airdrop")?;
    let client = reqwest::Client::new();
    let headers = remote_data::json_headers();
    let body = json!({ "address": node_address.to_string(), "code": code, });
    tracing::debug!(%url, ?headers, %body, "Posting funding tool");
    (|| async {
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

        let result = if status == reqwest::StatusCode::UNAUTHORIZED {
            let unauthorized: UnauthorizedError = resp.json().await.map_err(|err| {
                tracing::error!(?err, "Funding tool read unauthorized response failed");
                Error::from(err)
            })?;
            tracing::debug!(?unauthorized, "Funding tool unauthorized response");
            Ok(Some(unauthorized.error))
        } else {
            let text = resp.text().await.map_err(|err| {
                tracing::error!(?err, "Funding tool read response failed");
                Error::from(err)
            })?;

            tracing::debug!(%status, ?text, "Funding tool response");
            if status.is_success() {
                Ok(None)
            } else {
                Err(Error::FundingTool(text))
            }
        };
        // allow conversion to retry error
        let res = result?;
        Ok(res)
    })
    .retry(ExponentialBuilder::default())
    .await
}

async fn run_hopr(hopr_params: HoprParams, ticket_value: Balance<WxHOPR>) -> Result<Hopr, Error> {
    tracing::debug!("starting hopr runner");
    let cfg = hopr_params.to_config(ticket_value).await?;
    let keys = hopr_params.calc_keys().await?;
    Hopr::new(cfg, crate::hopr::config::db_file()?.as_path(), keys)
        .await
        .map_err(Error::from)
}

async fn run_fund_channel(
    hopr: Arc<Hopr>,
    address: Address,
    ticket_value: Balance<WxHOPR>,
) -> Result<(), hopr_api::ChannelError> {
    let amount = balance::funding_amount(ticket_value);
    let threshold = balance::min_stake_threshold(ticket_value);
    tracing::debug!(%address, %amount, %threshold, "starting fund channel runner");
    (|| async {
        hopr.ensure_channel_open_and_funded(address, amount, threshold).await?;
        Ok(())
    })
    .retry(ExponentialBuilder::default())
    .await
}

async fn run_monitor_session(hopr: Arc<Hopr>, session: &SessionClientMetadata) {
    tracing::debug!(?session, "starting session monitor runner");
    loop {
        let delay = rand::rng().random_range(5..10);
        time::sleep(Duration::from_secs(delay)).await;
        let sessions = hopr.list_sessions(IpProtocol::UDP).await;
        let found = sessions.iter().any(|s| s == session);
        if found {
            tracing::info!(?session, "session still active");
        } else {
            break;
        }
    }
}

impl Display for Results {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Results::FundChannel {
                address,
                res,
                target_dest,
            } => match res {
                Ok(_) => write!(
                    f,
                    "FundChannel (-> {} -> {}): Success",
                    log_output::address(address),
                    log_output::address(target_dest)
                ),
                Err(err) => write!(
                    f,
                    "FundChannel (-> {} -> {}): Error({})",
                    log_output::address(address),
                    log_output::address(target_dest),
                    err
                ),
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
                Ok(None) => write!(f, "FundingTool: Success"),
                Ok(Some(msg)) => write!(f, "FundingTool: Message({})", msg),
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
            Results::ConnectionResultPreWg { res } => match res {
                Ok(session) => write!(f, "ConnectionResultPreWg: Success ({})", session),
                Err(err) => write!(f, "ConnectionResultPreWg: Error({})", err),
            },
            Results::ConnectionResultPostWg { res } => match res {
                Ok(_) => write!(f, "ConnectionResultPostWg: Success"),
                Err(err) => write!(f, "ConnectionResultPostWg: Error({})", err),
            },
            Results::DisconnectionEvent { wg_public_key, evt } => {
                write!(f, "DisconnectionEvent ({}): {}", wg_public_key, evt)
            }
            Results::DisconnectionResult { wg_public_key, res } => match res {
                Ok(_) => write!(f, "DisconnectionResult ({}): Success", wg_public_key),
                Err(err) => write!(f, "DisconnectionResult ({}): Error({})", wg_public_key, err),
            },
            Results::SessionMonitorFailed => write!(f, "SessionMonitorFailed"),
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
