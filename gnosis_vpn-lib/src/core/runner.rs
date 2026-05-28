//! Various runner tasks that might get extracted into their own modules once applicable.
//! These function expect to be spawn and will deliver their result or progress via channels.

use backon::{ExponentialBuilder, Retryable};
use bytesize::ByteSize;
use edgli::blokli::{IncentiveOperations, make_incentive_operations};
use edgli::hopr_lib::api::node::HoprState;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::builder::Keypair;
use edgli::hopr_lib::exports::network::types::types::IpProtocol;
use edgli::hopr_lib::exports::transport::SurbBalancerConfig;
use edgli::{BlockchainConnectorConfig, EdgliInitState};
use human_bandwidth::re::bandwidth::Bandwidth;
use rand::prelude::*;
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use url::Url;

use std::fmt::{self, Display};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::command::{self, Response};
use crate::compat::SafeModule;
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError, config as hopr_config};
use crate::route_health::{self, HealthCheckOutcome};
use crate::worker_params::{self, WorkerParams};
use crate::{balance, connection, event, ping, remote_data};

/// Results indicate events that arise from concurrent runners.
/// These runners are usually spawned and want to report data or progress back to the core application loop.
pub enum Results {
    NodeBalance {
        res: Result<balance::PreSafe, Error>,
    },
    QuerySafe {
        res: Result<Option<SafeModule>, Error>,
    },
    DeploySafe {
        res: Result<SafeModule, Error>,
    },
    MinimumBalanceRecommendation {
        res: Result<balance::BalanceRecommendation, Error>,
    },
    IdealBalanceRecommendation {
        res: Result<balance::BalanceRecommendation, Error>,
    },
    CapacityAllocations {
        res: Result<std::collections::HashMap<balance::CapacityAllocator, balance::Capacity>, Error>,
    },
    Balances {
        res: Result<balance::Balances, Error>,
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
    IncentiveOperations {
        res: Result<Arc<dyn IncentiveOperations>, Error>,
    },
    IncentiveOperationsRetry {
        error: String,
    },
    NodeWxhoprWithdraw {
        res: Result<(), Error>,
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
    TunnelPingResult {
        rtt: Result<Duration, String>,
    },
    HealthCheck {
        id: String,
        outcome: HealthCheckOutcome,
    },
    RetryReactor,
    NerdStatsTicketStats {
        res: command::TicketStatsStatus,
        resp: oneshot::Sender<Response>,
    },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    WorkerParams(#[from] worker_params::Error),
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
    #[error("Funding tool error: {0}")]
    FundingTool(String),
    #[error("IncentiveOperations creation error: {0}")]
    IncentiveOperationsCreation(String),
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

pub async fn minimum_balance_recommendation(
    incentive_operations: Arc<dyn IncentiveOperations>,
    cfg: edgli::strategy::IncentiveConfiguration,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_minimum_balance_recommendation(incentive_operations, cfg).await;
    let _ = results_sender.send(Results::MinimumBalanceRecommendation { res }).await;
}

pub async fn ideal_balance_recommendation(
    hopr: Arc<Hopr>,
    cfg: edgli::strategy::IncentiveConfiguration,
    results_sender: mpsc::Sender<Results>,
) {
    let res = hopr
        .ideal_balance_recommendation(&cfg)
        .await
        .map_err(|e| Error::Chain(e.to_string()));
    let _ = results_sender.send(Results::IdealBalanceRecommendation { res }).await;
}

pub async fn capacity_allocations(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    let res = hopr
        .capacity_allocations()
        .await
        .map_err(|e| Error::Chain(e.to_string()));
    let _ = results_sender.send(Results::CapacityAllocations { res }).await;
}

pub async fn balances(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    tracing::debug!("starting balances runner");
    let res = hopr.balances().await.map_err(Error::from);
    let _ = results_sender.send(Results::Balances { res }).await;
}

pub async fn node_balance(incentive_operations: Arc<dyn IncentiveOperations>, results_sender: mpsc::Sender<Results>) {
    let res = run_node_balance(incentive_operations).await;
    let _ = results_sender.send(Results::NodeBalance { res }).await;
}

pub async fn query_safe(incentive_operations: Arc<dyn IncentiveOperations>, results_sender: mpsc::Sender<Results>) {
    let res = run_query_safe(incentive_operations).await;
    let _ = results_sender.send(Results::QuerySafe { res }).await;
}

pub async fn funding_tool(worker_params: WorkerParams, code: String, results_sender: mpsc::Sender<Results>) {
    let res = run_funding_tool(worker_params, code).await;
    let _ = results_sender.send(Results::FundingTool { res }).await;
}

pub async fn safe_deployment(
    incentive_operations: Arc<dyn IncentiveOperations>,
    presafe: balance::PreSafe,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_safe_deployment(incentive_operations, presafe).await;
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

pub async fn hopr(
    worker_params: WorkerParams,
    blokli_config: BlokliConfig,
    safe_module: &SafeModule,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_hopr(worker_params, blokli_config, safe_module, &results_sender).await;
    let _ = results_sender
        .send(Results::Hopr {
            res,
            safe_module: safe_module.clone(),
        })
        .await;
}

pub async fn node_wxhopr_withdraw(
    incentive_operations: Arc<dyn IncentiveOperations>,
    safe_address: Address,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_node_wxhopr_withdraw(incentive_operations, safe_address).await;
    let _ = results_sender.send(Results::NodeWxhoprWithdraw { res }).await;
}

pub async fn wait_for_running(hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>) {
    while hopr.status() != HoprState::Running {
        time::sleep(Duration::from_secs(1)).await;
    }
    let _ = results_sender.send(Results::HoprRunning).await;
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

pub async fn tunnel_ping_loop(interval: Duration, sender: mpsc::Sender<Results>) {
    let ping_opts = ping::Options {
        seq_count: 1,
        ..Default::default()
    };
    let ping_timeout = ping_opts.timeout;

    tracing::debug!(?interval, "starting tunnel ping probe");

    loop {
        time::sleep(route_health::jitter(interval)).await;

        let (tx, rx) = oneshot::channel();
        let request = Results::ConnectionRequestToRoot(event::RunnerToRoot::Ping {
            options: ping_opts.clone(),
            resp: tx,
        });
        if sender.send(request).await.is_err() {
            break;
        }

        let rtt = match time::timeout(ping_timeout * 2, rx).await {
            Ok(Ok(Ok(rtt))) => Ok(rtt),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err("ping response channel closed".to_string()),
            Err(_) => Err("ping response timed out".to_string()),
        };

        if sender.send(Results::TunnelPingResult { rtt }).await.is_err() {
            break;
        }
    }
}

pub async fn create_incentive_operations(
    worker_params: &WorkerParams,
    blokli_config: BlockchainConnectorConfig,
    results_sender: mpsc::Sender<Results>,
) {
    let res = run_create_incentive_operations(worker_params, blokli_config, results_sender.clone()).await;
    let _ = results_sender.send(Results::IncentiveOperations { res }).await;
}

async fn run_node_wxhopr_withdraw(
    incentive_operations: Arc<dyn IncentiveOperations>,
    safe_address: Address,
) -> Result<(), Error> {
    (|| {
        let incentive_operations = incentive_operations.clone();
        async move {
            let (wxhopr, _xdai) = incentive_operations
                .balances()
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;
            if !wxhopr.is_zero() {
                tracing::info!(%wxhopr, %safe_address, "withdrawing node wxHOPR to safe");
                incentive_operations
                    .withdraw_wxhopr(safe_address, wxhopr)
                    .await
                    .map_err(|e| Error::Chain(e.to_string()))?;
            }
            Ok(())
        }
    })
    .retry(
        ExponentialBuilder::new()
            .with_min_delay(Duration::from_secs(10))
            .with_max_delay(Duration::from_secs(60))
            .with_factor(2.0)
            .with_jitter()
            .without_max_times(),
    )
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "wxHOPR withdrawal attempt failed, retrying...");
    })
    .await
}

async fn run_query_safe(incentive_operations: Arc<dyn IncentiveOperations>) -> Result<Option<SafeModule>, Error> {
    tracing::debug!("starting query safe runner");
    (|| {
        let ops = incentive_operations.clone();
        async move {
            ops.retrieve_safe()
                .await
                .map_err(|e| Error::Chain(e.to_string()))
                .map(|b| b.map(SafeModule::from))
        }
    })
    .retry(remote_data::backoff_expo_long_delay())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Safe query attempt failed, retrying...");
    })
    .await
}

async fn run_node_balance(incentive_operations: Arc<dyn IncentiveOperations>) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting node balance runner");
    (|| {
        let ops = incentive_operations.clone();
        async move {
            let (balance_wxhopr, balance_xdai) = ops.balances().await.map_err(|e| Error::Chain(e.to_string()))?;
            Ok(balance::PreSafe {
                node_xdai: balance_xdai,
                node_wxhopr: balance_wxhopr,
            })
        }
    })
    .retry(remote_data::backoff_expo_long_delay())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "PreSafe attempt failed, retrying...");
    })
    .await
}

async fn run_minimum_balance_recommendation(
    incentive_operations: Arc<dyn IncentiveOperations>,
    cfg: edgli::strategy::IncentiveConfiguration,
) -> Result<balance::BalanceRecommendation, Error> {
    tracing::debug!("starting minimum balance recommendation runner");
    (|| {
        let ops = incentive_operations.clone();
        async move {
            let rec = edgli::strategy::minimum_balance_recommendation(&*ops, &cfg)
                .await
                .map_err(|e| Error::Chain(e.to_string()))?;
            Ok(balance::BalanceRecommendation {
                wxhopr: rec.wxhopr,
                xdai: rec.xdai,
            })
        }
    })
    .retry(remote_data::backoff_expo_long_delay())
    .notify(|err, delay| {
        tracing::warn!(
            ?err,
            ?delay,
            "Minimum balance recommendation attempt failed, retrying..."
        );
    })
    .await
}

async fn run_safe_deployment(
    incentive_operations: Arc<dyn IncentiveOperations>,
    presafe: balance::PreSafe,
) -> Result<SafeModule, Error> {
    tracing::debug!("starting safe deployment runner");
    (|| {
        let ops = incentive_operations.clone();
        async move {
            ops.deploy_safe(presafe.node_wxhopr)
                .await
                .map_err(|e| Error::Chain(e.to_string()))
                .map(SafeModule::from)
        }
    })
    .retry(remote_data::backoff_expo_long_delay())
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
    .retry(remote_data::backoff_expo_long_delay())
    .notify(|err, delay| {
        tracing::warn!(?err, ?delay, "Funding tool attempt failed, retrying...");
    })
    .await
}

async fn run_hopr(
    worker_params: WorkerParams,
    blokli_config: BlokliConfig,
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

    Hopr::new(cfg, keys, blokli_url, blokli_config.into(), visitor)
        .await
        .map_err(Error::from)
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

async fn run_create_incentive_operations(
    worker_params: &WorkerParams,
    blokli_config: BlockchainConnectorConfig,
    results_sender: mpsc::Sender<Results>,
) -> Result<Arc<dyn IncentiveOperations>, Error> {
    let blokli_provider = worker_params.blokli_url();
    let chain_key = worker_params.calc_keys().await?.chain_key;
    (|| async {
        let ops = make_incentive_operations(blokli_provider.clone(), &chain_key, Some(blokli_config))
            .await
            .map_err(|e| Error::IncentiveOperationsCreation(e.to_string()))?;
        Ok(Arc::from(ops))
    })
    .retry(remote_data::backoff_expo_long_delay())
    .notify(move |err: &Error, delay| {
        tracing::warn!(?err, ?delay, "IncentiveOperations creation attempt failed, retrying...");
        let sender = results_sender.clone();
        let error = err.to_string();
        tokio::spawn(async move {
            let _ = sender.send(Results::IncentiveOperationsRetry { error }).await;
        });
    })
    .await
}

impl Display for Results {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Results::HoprConstruction(state) => write!(f, "HoprConstruction: {:?}", state),
            Results::NodeBalance { res } => match res {
                Ok(presafe) => write!(f, "NodeBalance: {}", presafe),
                Err(err) => write!(f, "NodeBalance: Error({})", err),
            },
            Results::MinimumBalanceRecommendation { res } => match res {
                Ok(rec) => write!(
                    f,
                    "MinimumBalanceRecommendation: wxHOPR >= {}, xDAI >= {}",
                    rec.wxhopr, rec.xdai
                ),
                Err(err) => write!(f, "MinimumBalanceRecommendation: Error({})", err),
            },
            Results::IdealBalanceRecommendation { res } => match res {
                Ok(rec) => write!(
                    f,
                    "IdealBalanceRecommendation: wxHOPR >= {}, xDAI >= {}",
                    rec.wxhopr, rec.xdai
                ),
                Err(err) => write!(f, "IdealBalanceRecommendation: Error({})", err),
            },
            Results::CapacityAllocations { res } => match res {
                Ok(map) => write!(f, "CapacityAllocations: {} entries", map.len()),
                Err(err) => write!(f, "CapacityAllocations: Error({})", err),
            },
            Results::Balances { res } => match res {
                Ok(balances) => write!(f, "Balances: {}", balances),
                Err(err) => write!(f, "Balances: Error({})", err),
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
            Results::NodeWxhoprWithdraw { res } => match res {
                Ok(()) => write!(f, "NodeWxhoprWithdraw: Success"),
                Err(err) => write!(f, "NodeWxhoprWithdraw: Error({})", err),
            },
            Results::ConnectedPeers { res } => match res {
                Ok(peers) => write!(f, "ConnectedPeers: {:?}", peers),
                Err(err) => write!(f, "ConnectedPeers: Error({})", err),
            },
            Results::IncentiveOperations { res } => match res {
                Ok(_) => write!(f, "IncentiveOperations: Created Successfully"),
                Err(err) => write!(f, "IncentiveOperations: Error({})", err),
            },
            Results::IncentiveOperationsRetry { error } => {
                write!(f, "IncentiveOperationsRetry: Error({})", error)
            }
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
            Results::TunnelPingResult { rtt } => match rtt {
                Ok(d) => write!(f, "TunnelPingResult: {:.1}ms", d.as_secs_f64() * 1000.0),
                Err(err) => write!(f, "TunnelPingResult: Error({})", err),
            },
            Results::QuerySafe { res } => match res {
                Ok(Some(_)) => write!(f, "QuerySafe: Safe found"),
                Ok(None) => write!(f, "QuerySafe: No safe found"),
                Err(err) => write!(f, "QuerySafe: Error({})", err),
            },
            Results::HealthCheck { id, outcome } => write!(f, "HealthCheck ({}): {:?}", id, outcome),
            Results::RetryReactor => write!(f, "RetryReactor"),
            Results::NerdStatsTicketStats { .. } => write!(f, "NerdStatsTicketStats"),
        }
    }
}

pub fn to_surb_balancer_config(
    response_buffer: ByteSize,
    max_surb_upstream: Bandwidth,
) -> Result<SurbBalancerConfig, SurbConfigError> {
    // Buffer worth at least 2 reply packets
    if response_buffer.as_u64() < 2 * edgli::hopr_lib::exports::transport::SESSION_MTU as u64 {
        return Err(SurbConfigError::ResponseBufferTooSmall);
    }
    if max_surb_upstream.is_zero() {
        return Err(SurbConfigError::MaxSurbUpstreamCannotBeZero);
    }
    let config = SurbBalancerConfig {
        target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::exports::transport::SESSION_MTU as u64,
        max_surbs_per_sec: (max_surb_upstream.as_bps() as usize / (8 * edgli::hopr_lib::exports::transport::SURB_SIZE))
            as u64,
        ..Default::default()
    };
    Ok(config)
}
