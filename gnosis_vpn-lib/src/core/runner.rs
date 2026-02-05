//! Various runner tasks that might get extracted into their own modules once applicable.
//! These function expect to be spawn and will deliver their result or progress via channels.

use backon::{ExponentialBuilder, Retryable};
use bytesize::ByteSize;
use edgli::EdgliInitState;
use edgli::blokli::SafelessInteractor;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Address, Balance, WxHOPR};
use edgli::hopr_lib::{IpProtocol, SurbBalancerConfig};
use human_bandwidth::re::bandwidth::Bandwidth;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time;
use url::Url;

use std::fmt::{self, Display};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::balance;
use crate::compat::SafeModule;
use crate::connection;
use crate::health::{self, Health};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, api as hopr_api, config as hopr_config};
use crate::log_output;
use crate::ticket_stats::{self, TicketStats};
use crate::worker_params::{self, WorkerParams};
use crate::{event, remote_data};

/// Results indicate events that arise from concurrent runners.
/// These runners are usually spawned and want to report data or progress back to the core application loop.
pub enum Results {
    FundChannel {
        address: Address,
        res: Result<(), hopr_api::ChannelError>,
    },
    NodeBalance {
        res: Result<balance::PreSafe, Error>,
    },
    QuerySafe {
        res: Result<Option<SafeModule>, Error>,
    },
    DeploySafe {
        res: Result<SafeModule, Error>,
    },
    TicketStats {
        res: Result<TicketStats, Error>,
    },
    PersistSafe {
        res: Result<(), hopr_config::Error>,
        safe_module: SafeModule,
    },
    FundingTool {
        res: Result<Option<String>, Error>,
    },
    Hopr {
        res: Result<Hopr, Error>,
        safe_module: SafeModule,
    },
    Balances {
        res: Result<balance::Balances, Error>,
    },
    ConnectedPeers {
        res: Result<Vec<Address>, Error>,
    },
    HoprConstruction(EdgliInitState),
    HoprRunning,
    ConnectionEvent(connection::up::Event),
    ConnectionRequestToRoot(event::RunnerToRoot),
    ConnectionResult {
        res: Result<SessionClientMetadata, connection::up::Error>,
    },
    DisconnectionEvent {
        wg_public_key: String,
        evt: connection::down::Event,
    },
    DisconnectionResult {
        wg_public_key: String,
        res: Result<(), connection::down::Error>,
    },
    SessionMonitorFailed,
    HealthResult {
        res: Result<Health, health::Error>,
    },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    WorkerParams(#[from] worker_params::Error),
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

#[derive(Debug, Error)]
pub enum SurbConfigError {
    #[error("Response buffer byte size too small")]
    ResponseBufferTooSmall,
    #[error("Max SURB upstream bandwidth cannot be zero")]
    MaxSurbUpstreamCannotBeZero,
}

#[derive(Debug, Deserialize)]
struct UnauthorizedError {
    error: String,
}

pub async fn ticket_stats(safeless_interactor: Arc<SafelessInteractor>, results_sender: mpsc::Sender<Results>) {
    let res = run_ticket_stats(safeless_interactor).await;
    let _ = results_sender.send(Results::TicketStats { res }).await;
}

pub async fn node_balance(safeless_interactor: Arc<SafelessInteractor>, results_sender: mpsc::Sender<Results>) {
    let res = run_node_balance(safeless_interactor).await;
    let _ = results_sender.send(Results::NodeBalance { res }).await;
}

pub async fn query_safe(safeless_interactor: Arc<SafelessInteractor>, results_sender: mpsc::Sender<Results>) {
    let res = run_query_safe(safeless_interactor).await;
    let _ = results_sender.send(Results::QuerySafe { res }).await;
}

pub async fn funding_tool(worker_params: WorkerParams, code: String, results_sender: mpsc::Sender<Results>) {
    let res = run_funding_tool(worker_params, code).await;
    let _ = results_sender.send(Results::FundingTool { res }).await;
}

pub async fn safe_deployment(
    safeless_interactor: Arc<SafelessInteractor>,
    presafe: balance::PreSafe,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_safe_deployment(safeless_interactor, presafe).await;
    let _ = results_sender.send(Results::DeploySafe { res }).await;
}

pub async fn persist_safe(state_home: PathBuf, safe_module: SafeModule, results_sender: mpsc::Sender<Results>) {
    tracing::debug!("persisting safe module");
    let res = hopr_config::store_safe(state_home, &safe_module).await;
    let _ = results_sender
        .send(Results::PersistSafe {
            res,
            safe_module: safe_module.clone(),
        })
        .await;
}

pub async fn hopr(worker_params: WorkerParams, safe_module: &SafeModule, results_sender: mpsc::Sender<Results>) {
    let res = run_hopr(worker_params, safe_module, &results_sender).await;
    let _ = results_sender
        .send(Results::Hopr {
            res,
            safe_module: safe_module.clone(),
        })
        .await;
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
    tracing::debug!("starting connected peers runner");
    let res = hopr.connected_peers().await.map_err(Error::from);
    let _ = results_sender.send(Results::ConnectedPeers { res }).await;
}

pub async fn monitor_session(hopr: Arc<Hopr>, session: &SessionClientMetadata, results_sender: mpsc::Sender<Results>) {
    run_monitor_session(hopr, session).await;
    let _ = results_sender.send(Results::SessionMonitorFailed).await;
}

async fn run_query_safe(safeless_interactor: Arc<SafelessInteractor>) -> Result<Option<SafeModule>, Error> {
    tracing::debug!("starting query safe runner");
    (|| {
        let blokli = safeless_interactor.clone();
        async move {
            blokli
                .retrieve_safe()
                .await
                .map_err(|e| Error::Chain(e.to_string()))
                .map(|b| b.map(SafeModule::from))
        }
    })
    .retry(ExponentialBuilder::default())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Safe query attempt failed, retrying...");
    })
    .await
}

async fn run_node_balance(safeless_interactor: Arc<SafelessInteractor>) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting node balance runner");
    (|| {
        let blokli = safeless_interactor.clone();
        async move {
            let (balance_wxhopr, balance_xdai) = blokli.balances().await.map_err(|e| Error::Chain(e.to_string()))?;
            Ok(balance::PreSafe {
                node_xdai: balance_xdai,
                node_wxhopr: balance_wxhopr,
            })
        }
    })
    .retry(ExponentialBuilder::default())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "PreSafe attempt failed, retrying...");
    })
    .await
}

async fn run_ticket_stats(safeless_interactor: Arc<SafelessInteractor>) -> Result<TicketStats, Error> {
    tracing::debug!("starting ticket stats runner");
    (|| {
        let blokli = safeless_interactor.clone();
        async move {
            let ticket_stats = blokli.ticket_stats().await.map_err(|e| Error::Chain(e.to_string()))?;

            Ok(TicketStats {
                ticket_price: ticket_stats.ticket_price,
                winning_probability: ticket_stats.winning_probability,
            })
        }
    })
    .retry(ExponentialBuilder::default())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Ticket stats attempt failed, retrying...");
    })
    .await
}

async fn run_safe_deployment(
    safeless_interactor: Arc<SafelessInteractor>,
    presafe: balance::PreSafe,
) -> Result<SafeModule, Error> {
    tracing::debug!("starting safe deployment runner");
    (|| {
        let blokli = safeless_interactor.clone();
        async move {
            blokli
                .deploy_safe(presafe.node_wxhopr)
                .await
                .map_err(|e| Error::Chain(e.to_string()))
                .map(SafeModule::from)
        }
    })
    .retry(ExponentialBuilder::default())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Safe deployment attempt failed, retrying...");
    })
    .await
}

// Posts to the HOPR funding tool API to request an airdrop using the provided code.
// Returns final errors in ok branch to break exponential backoff retries.
async fn run_funding_tool(worker_params: WorkerParams, code: String) -> Result<Option<String>, Error> {
    let keys = worker_params.calc_keys().await?;
    let node_address = keys.chain_key.public().to_address();
    let url = Url::parse("https://cfp-funding-api-656686060169.europe-west1.run.app/api/cfp-funding-tool/airdrop")?;
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
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Funding tool attempt failed, retrying...");
    })
    .await
}

async fn run_hopr(
    worker_params: WorkerParams,
    safe_module: &SafeModule,
    results_sender: &mpsc::Sender<Results>,
) -> Result<Hopr, Error> {
    tracing::debug!("starting hopr runner");
    let cfg = worker_params.to_config(safe_module).await?;
    let keys = worker_params.calc_keys().await?;
    let blokli_url = worker_params.blokli_url();
    let sender = results_sender.clone();
    let visitor = move |state| {
        if let Err(err) = sender.try_send(Results::HoprConstruction(state)) {
            tracing::warn!(?err, "Failed to send HOPR construction state update");
        }
    };

    Hopr::new(
        cfg,
        hopr::config::db_file(worker_params.state_home())?.as_path(),
        keys,
        blokli_url,
        visitor,
    )
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
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Fund channel attempt failed, retrying...");
    })
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
            Results::FundChannel { address, res } => match res {
                Ok(_) => write!(f, "FundChannel (-> {} ->): Success", log_output::address(address),),
                Err(err) => write!(
                    f,
                    "FundChannel (-> {} ->): Error({})",
                    log_output::address(address),
                    err
                ),
            },
            Results::HoprConstruction(state) => write!(f, "HoprConstruction: {:?}", state),
            Results::NodeBalance { res } => match res {
                Ok(presafe) => write!(f, "NodeBalance: {}", presafe),
                Err(err) => write!(f, "NodeBalance: Error({})", err),
            },
            Results::TicketStats { res } => match res {
                Ok(stats) => write!(f, "TicketStats: {}", stats),
                Err(err) => write!(f, "TicketStats: Error({})", err),
            },
            Results::DeploySafe { res } => match res {
                Ok(deployment) => write!(f, "DeploySafe: {:?}", deployment),
                Err(err) => write!(f, "DeploySafe: Error({})", err),
            },
            Results::PersistSafe { res, safe_module: _ } => match res {
                Ok(_) => write!(f, "PersistSafe: Success"),
                Err(err) => write!(f, "PersistSafe: Error({})", err),
            },
            Results::FundingTool { res } => match res {
                Ok(None) => write!(f, "FundingTool: Success"),
                Ok(Some(msg)) => write!(f, "FundingTool: Message({})", msg),
                Err(err) => write!(f, "FundingTool: Error({})", err),
            },
            Results::Hopr { res, safe_module: _ } => match res {
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
            Results::ConnectionEvent(evt) => {
                write!(f, "ConnectionEvent: {}", evt)
            }
            Results::ConnectionRequestToRoot(req) => {
                write!(f, "ConnectionRequestToRoot: {:?}", req)
            }
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
            Results::SessionMonitorFailed => write!(f, "SessionMonitorFailed"),
            Results::QuerySafe { res } => match res {
                Ok(Some(_)) => write!(f, "QuerySafe: Safe found"),
                Ok(None) => write!(f, "QuerySafe: No safe found"),
                Err(err) => write!(f, "QuerySafe: Error({})", err),
            },
        }
    }
}

pub fn to_surb_balancer_config(
    response_buffer: ByteSize,
    max_surb_upstream: Bandwidth,
) -> Result<SurbBalancerConfig, SurbConfigError> {
    // Buffer worth at least 2 reply packets
    if response_buffer.as_u64() < 2 * edgli::hopr_lib::SESSION_MTU as u64 {
        return Err(SurbConfigError::ResponseBufferTooSmall);
    }
    if max_surb_upstream.is_zero() {
        return Err(SurbConfigError::MaxSurbUpstreamCannotBeZero);
    }
    let config = SurbBalancerConfig {
        target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::SESSION_MTU as u64,
        max_surbs_per_sec: (max_surb_upstream.as_bps() as usize / (8 * edgli::hopr_lib::SURB_SIZE)) as u64,
        ..Default::default()
    };
    Ok(config)
}
