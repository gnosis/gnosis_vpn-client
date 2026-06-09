use edgli::EdgliInitState;
use edgli::blokli::IncentiveOperations;
use edgli::hopr_lib::builder::Keypair;
use edgli::hopr_lib::exports::transport::SessionId;
use futures_util::future::AbortHandle;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::collections::{HashMap, HashSet};
use std::net;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::command::{self, Response, RunMode, WorkerCommand};
use crate::compat::SafeModule;
use crate::config::{self, Config};
use crate::connection;
use crate::connection::destination::{Address, Destination};
use crate::connection::pseudonym_cache::PseudonymCache;
use crate::event::{CoreToWorker, RequestToRoot, ResponseFromRoot, RunnerToRoot, WorkerToCore};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError, config as hopr_config, identity};
use crate::route_health::{self, RouteHealth};
use crate::worker_params::{self, WorkerParams};
use crate::{balance, log_output, ticket_stats, wireguard};

pub(crate) mod runner;

use runner::Results;

enum Responder {
    Unit(oneshot::Sender<Result<(), String>>),
    Str(oneshot::Sender<Result<String, String>>),
    Duration(oneshot::Sender<Result<Duration, String>>),
}

const NODE_WXHOPR_WITHDRAW_INTERVAL: Duration = Duration::from_secs(45);
/// How long a peer IP remains in the killswitch allowlist after it was last seen in an
/// `announced_peers` snapshot. Prevents churn during brief libp2p reconnects.
const PEER_IP_HYSTERESIS_SECS: u64 = 300;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(#[from] config::Error),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("HOPR error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Hopr config error: {0}")]
    HoprConfig(#[from] hopr_config::Error),
    #[error("Hopr identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("Hopr params error: {0}")]
    HoprParams(#[from] worker_params::Error),
    #[error("IncentiveOperations creation error: {0}")]
    IncentiveOperationsCreation(String),
}

pub struct Core {
    // config data
    config: Config,

    // static data
    worker_params: WorkerParams,
    node_address: Address,
    outgoing_sender: mpsc::Sender<CoreToWorker>,
    incoming_receiver: mpsc::Receiver<WorkerToCore>,

    // cancellation tokens
    cancel_connection: CancellationToken,
    cancel_node_wxhopr: CancellationToken,
    cancel_on_shutdown: CancellationToken,
    cancel_presafe_queries: CancellationToken,

    // user provided data
    target_destination: Option<Destination>,

    // runtime data
    phase: Phase,
    incentive_operations: Option<Arc<dyn IncentiveOperations>>,
    hopr: Option<Arc<Hopr>>,
    minimum_balance_recommendation: Option<balance::BalanceRecommendation>,
    ideal_balance_recommendation: Option<balance::BalanceRecommendation>,
    capacity_allocations: Option<HashMap<balance::CapacityAllocator, balance::Capacity>>,
    balances: Option<balance::Balances>,
    strategy_handle: Option<AbortHandle>,
    route_healths: HashMap<String, RouteHealth>,
    next_request_id: u64,
    // Maps a request_id to the oneshot sender waiting for root's response.
    // request_id is needed even though at most one request is in-flight at a time:
    // root runs pings in a JoinSet, so a stale ping from a cancelled connection
    // can arrive after a new responder has been stored — the id mismatch discards it.
    // Cleared in disconnect_from_connection so stale entries don't outlive their connection.
    responders: HashMap<u64, Responder>,
    ongoing_disconnections: Vec<connection::down::Down>,
    cached_resolved_blokli_ips: Vec<net::Ipv4Addr>,
    pseudonym_cache: PseudonymCache,
    /// Last time each peer IP was seen in an `announced_peers` snapshot.
    /// IPs are retained in the killswitch allowlist for PEER_IP_HYSTERESIS_SECS after
    /// they last appeared — avoids flapping during brief libp2p churn.
    peer_ip_last_seen: HashMap<net::Ipv4Addr, Instant>,
    last_sent_peer_ips: Vec<net::Ipv4Addr>,
}

#[derive(Debug, Clone)]
enum Phase {
    // initial phase — create the IncentiveOperations handle (Blokli-backed)
    // and determine if a Safe has been deployed for this node
    Initial {
        last_error: Option<String>,
    },
    /// safe absent or safe deployment error - repeatedly query node balance and safe info
    CheckingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
        funding_tool: balance::FundingTool,
        deploy_safe_error: Option<String>,
    },
    /// enough funds and no deployed safe - run safe deployment
    DeployingSafe {
        node_balance: Querying<balance::PreSafe>,
        query_safe: Querying<Option<SafeModule>>,
    },
    /// construct edge client
    Starting {
        edgli_init_state: Option<EdgliInitState>,
        last_error: Option<String>,
    },
    /// start edge client
    HoprSyncing,
    /// edge client running normally
    HoprRunning,
    // connecting to a destination
    Connecting(connection::up::Up),
    /// connected to a destination
    Connected(connection::up::Up),
    /// dismantle state
    ShuttingDown,
}

#[derive(Debug, Clone)]
enum Querying<T> {
    Init,
    Success(T),
    Error(String),
}

impl Core {
    pub async fn init(
        config: Config,
        worker_params: WorkerParams,
        target_dest_id: Option<String>,
        outgoing_sender: mpsc::Sender<CoreToWorker>,
    ) -> Result<(Core, mpsc::Sender<WorkerToCore>), Error> {
        wireguard::available().await?;
        wireguard::executable().await?;
        let keys = worker_params.persist_identity_generation().await?;
        let node_address = keys.chain_key.public().to_address();
        let cancel_on_shutdown = CancellationToken::new();
        let mut route_healths = HashMap::new();
        for (id, dest) in config.destinations.clone() {
            route_healths.insert(
                id,
                RouteHealth::new(&dest, worker_params.allow_insecure(), cancel_on_shutdown.clone()),
            );
        }

        let target_destination = target_dest_id.and_then(|id| config.destinations.get(&id).cloned());

        let (incoming_sender, incoming_receiver) = mpsc::channel(32);
        let cached_resolved_blokli_ips = worker_params.cached_blokli_ips().to_vec();
        let pseudonym_cache = PseudonymCache::new(config.connection.session_pseudonym_ttl);
        let core = Core {
            // config data
            config,

            // static data
            worker_params,
            node_address,
            outgoing_sender,
            incoming_receiver,

            // cancellation tokens
            cancel_connection: cancel_on_shutdown.child_token(),
            cancel_node_wxhopr: cancel_on_shutdown.child_token(),
            cancel_on_shutdown: cancel_on_shutdown.clone(),
            cancel_presafe_queries: cancel_on_shutdown.child_token(),

            // user provided data
            target_destination,

            // runtime data
            phase: Phase::Initial { last_error: None },
            hopr: None,
            incentive_operations: None,
            minimum_balance_recommendation: None,
            ideal_balance_recommendation: None,
            capacity_allocations: None,
            balances: None,
            strategy_handle: None,
            ongoing_disconnections: Vec::new(),
            route_healths,
            next_request_id: 0,
            responders: HashMap::new(),
            // needed to keep working during enabled killswitch
            cached_resolved_blokli_ips,
            pseudonym_cache,
            peer_ip_last_seen: HashMap::new(),
            last_sent_peer_ips: Vec::new(),
        };
        Ok((core, incoming_sender))
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    pub async fn start(mut self) {
        let (results_sender, mut results_receiver) = mpsc::channel(32);
        self.spawn_initial_runner(&results_sender, Duration::ZERO);
        loop {
            tokio::select! {
                // React to an incoming worker events
                Some(event) = self.incoming_receiver.recv() => {
                    if self.on_event(event, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
                }

                // React to internal results from spawned runner tasks
                Some(results) = results_receiver.recv() => {
                    if self.on_results(results, &results_sender).await {
                        continue;
                    } else {
                        break;
                    }
                }

                else => {
                    tracing::warn!("event receiver closed");
                    break;
                }
            }
        }
    }

    /// receive an event from the worker main thread
    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    async fn on_event(&mut self, event: WorkerToCore, results_sender: &mpsc::Sender<Results>) -> bool {
        match event {
            WorkerToCore::Shutdown => {
                tracing::debug!("incoming shutdown request");
                self.phase = Phase::ShuttingDown;
                // no need to recreate cancellation tokens after shutdown
                self.cancel_on_shutdown.cancel();
                if let Some(hopr) = self.hopr.clone() {
                    let shutdown_tracker = TaskTracker::new();
                    if let Some(handle) = self.strategy_handle.take() {
                        shutdown_tracker.spawn(async move {
                            tracing::debug!("aborting strategy task");
                            handle.abort();
                        });
                    }
                    shutdown_tracker.spawn(async move {
                        tracing::debug!("shutting down hopr");
                        hopr.shutdown().await;
                    });
                    shutdown_tracker.close();
                    shutdown_tracker.wait().await;
                }
                false
            }

            WorkerToCore::ResponseFromRoot(resp) => {
                tracing::debug!(?resp, "incoming response from root");
                match resp {
                    ResponseFromRoot::KillswitchLockdown { request_id, res } => {
                        if let Some(Responder::Unit(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for killswitch lockdown response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for killswitch lockdown response (evicted or duplicate)"
                            );
                        }
                    }
                    ResponseFromRoot::DynamicWgRouting { request_id, res } => {
                        if let Some(Responder::Str(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for dynamic wg routing response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for dynamic wg routing response (evicted or duplicate)"
                            );
                        }
                    }
                    ResponseFromRoot::StaticWgRouting { request_id, res } => {
                        if let Some(Responder::Str(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for static wg routing response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for static wg routing response (evicted or duplicate)"
                            );
                        }
                    }
                    ResponseFromRoot::Ping { request_id, res } => {
                        if let Some(Responder::Duration(tx)) = self.responders.remove(&request_id) {
                            let _ = tx.send(res).map_err(|_| {
                                tracing::warn!("responder channel closed for ping response");
                            });
                        } else {
                            tracing::debug!(
                                request_id,
                                ?res,
                                "no responder for ping response (evicted or duplicate)"
                            );
                        }
                    }
                };

                true
            }

            WorkerToCore::WorkerCommand { cmd, resp } => {
                tracing::debug!(%cmd, "incoming command");
                match cmd {
                    WorkerCommand::NerdStats => {
                        tracing::debug!("incoming nerd stats request");
                        let Some(ops) = self.incentive_operations.clone() else {
                            let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse::NoInfo(
                                command::TicketStatsStatus::Waiting,
                            )));
                            return true;
                        };
                        let sender = results_sender.clone();
                        tokio::spawn(async move {
                            let ticket_stats_status = match ops.ticket_stats().await {
                                Ok(ts) => command::TicketStatsStatus::Available(ticket_stats::TicketStats {
                                    ticket_price: ts.ticket_price,
                                    winning_probability: ts.winning_probability.into(),
                                }),
                                Err(e) => command::TicketStatsStatus::Error(e.to_string()),
                            };
                            let _ = sender
                                .send(Results::NerdStatsTicketStats {
                                    res: ticket_stats_status,
                                    resp,
                                })
                                .await;
                        });
                    }

                    WorkerCommand::Status => {
                        let runmode = match self.phase.clone() {
                            Phase::Initial { last_error } => RunMode::Init { last_error },
                            Phase::CheckingSafe {
                                node_balance,
                                query_safe,
                                funding_tool,
                                deploy_safe_error,
                            } => {
                                let balance = match node_balance {
                                    Querying::Success(ref b) => Some(b.clone()),
                                    _ => None,
                                };
                                let mut errors = "".to_string();
                                if let Querying::Error(err) = node_balance {
                                    errors = err
                                };
                                if let Querying::Error(err) = query_safe {
                                    errors = format!("{} {}", errors, err);
                                }
                                if let Some(deploy_err) = deploy_safe_error {
                                    errors = format!("{} {}", errors, deploy_err);
                                }
                                let funding_tool = match funding_tool {
                                    balance::FundingTool::NotStarted => None,
                                    balance::FundingTool::InProgress => Some("Funding tool running".to_string()),
                                    balance::FundingTool::CompletedSuccess => {
                                        Some("Funding tool ran successfully".to_string())
                                    }
                                    balance::FundingTool::CompletedError(error) => {
                                        Some(format!("Funding tool error: {error}"))
                                    }
                                };
                                let error = if errors.is_empty() { None } else { Some(errors) };
                                RunMode::preparing_safe(
                                    self.node_address,
                                    &balance,
                                    funding_tool,
                                    error,
                                    self.minimum_balance_recommendation,
                                )
                            }
                            Phase::DeployingSafe {
                                node_balance: _,
                                query_safe: _,
                            } => RunMode::deploying_safe(self.node_address),
                            Phase::Starting {
                                edgli_init_state,
                                last_error,
                            } => RunMode::warmup(edgli_init_state, None, last_error),
                            Phase::HoprSyncing => RunMode::warmup(None, self.hopr.as_ref().map(|h| h.status()), None),
                            Phase::HoprRunning | Phase::Connecting(_) | Phase::Connected(_) => {
                                let funding_issues = match (
                                    &self.ideal_balance_recommendation,
                                    &self.capacity_allocations,
                                    &self.balances,
                                ) {
                                    (Some(ideal), Some(allocs), Some(bals)) => {
                                        Some(balance::to_funding_issues(*ideal, allocs, bals.node_xdai))
                                    }
                                    _ => None,
                                };
                                RunMode::running(self.hopr.as_ref().map(|h| h.status()), funding_issues)
                            }
                            Phase::ShuttingDown => RunMode::Shutdown,
                        };

                        let connecting = match &self.phase {
                            Phase::Connecting(conn) => Some(command::ConnectingInfo {
                                destination_id: conn.destination.id.clone(),
                                since: conn.phase.0,
                                phase: conn.phase.1.clone(),
                            }),
                            _ => None,
                        };
                        let connected = match &self.phase {
                            Phase::Connected(conn) => Some(command::ConnectedInfo {
                                destination_id: conn.destination.id.clone(),
                                since: conn.phase.0,
                            }),
                            _ => None,
                        };
                        let disconnecting = self
                            .ongoing_disconnections
                            .iter()
                            .map(|d| command::DisconnectingInfo {
                                destination_id: d.destination.id.clone(),
                                since: d.phase.0,
                                phase: d.phase.1.clone(),
                            })
                            .collect();
                        let mut vals = self.config.destinations.values().collect::<Vec<&Destination>>();
                        vals.sort_unstable_by(|a, b| a.id.cmp(&b.id));
                        let destinations = vals
                            .into_iter()
                            .map(|v| command::DestinationState {
                                destination: v.clone(),
                                route_health: self.route_healths.get(&v.id).map(command::RouteHealthView::from),
                            })
                            .collect();
                        let res = Response::status(command::StatusResponse {
                            run_mode: runmode,
                            destinations,
                            target_destination: self.target_destination.as_ref().map(|d| d.id.clone()),
                            connecting,
                            connected,
                            disconnecting,
                        });
                        let _ = resp.send(res);
                    }

                    WorkerCommand::Connect(id) => match self.config.destinations.clone().get(&id) {
                        Some(dest) => {
                            let is_already_active = match &self.phase {
                                Phase::Connected(conn) | Phase::Connecting(conn) => conn.destination == *dest,
                                _ => false,
                            };
                            if is_already_active {
                                let _ = resp.send(Response::connect(command::ConnectResponse::already_connected(
                                    dest.clone(),
                                )));
                            } else if let Some(rh) = self.route_healths.get(&dest.id) {
                                if rh.is_ready_to_connect() {
                                    let _ = resp
                                        .send(Response::connect(command::ConnectResponse::connecting(dest.clone())));
                                    self.target_destination = Some(dest.clone());
                                    self.act_on_target(results_sender);
                                } else if rh.is_unrecoverable() {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::unable(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                } else {
                                    let _ = resp.send(Response::connect(command::ConnectResponse::waiting(
                                        dest.clone(),
                                        rh.state().clone(),
                                    )));
                                    self.target_destination = Some(dest.clone());
                                }
                            } else {
                                tracing::warn!(%id, "no route health found for destination - this should not happen");
                                let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                            }
                        }
                        None => {
                            tracing::info!(%id, "cannot connect to destination - not configured");
                            let _ = resp.send(Response::connect(command::ConnectResponse::destination_not_found()));
                        }
                    },

                    WorkerCommand::Disconnect => {
                        self.target_destination = None;
                        self.cached_resolved_blokli_ips = Vec::new();
                        match self.phase.clone() {
                            Phase::Connected(conn) | Phase::Connecting(conn) => {
                                tracing::info!(current = %conn.destination, "disconnecting");
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::new(
                                    conn.destination.clone(),
                                )));
                            }
                            _ => {
                                tracing::debug!("no active connection to disconnect");
                                let _ = resp.send(Response::disconnect(command::DisconnectResponse::not_connected()));
                            }
                        }
                        self.act_on_target(results_sender);
                    }

                    WorkerCommand::Balance => {
                        let result = match (&self.hopr, &self.balances) {
                            (Some(hopr), Some(balances)) => {
                                let funding_issues =
                                    match (&self.ideal_balance_recommendation, &self.capacity_allocations) {
                                        (Some(ideal), Some(allocs)) => {
                                            Some(balance::to_funding_issues(*ideal, allocs, balances.node_xdai))
                                        }
                                        _ => None,
                                    };
                                Ok(command::BalanceResponse::build(
                                    &hopr.info(),
                                    balances,
                                    &self.config.destinations.clone(),
                                    self.capacity_allocations.as_ref(),
                                    self.ideal_balance_recommendation,
                                    funding_issues,
                                ))
                            }
                            _ => Err("balance data not yet available".to_string()),
                        };
                        let _ = resp.send(Response::Balance(result));
                    }

                    WorkerCommand::Telemetry => {
                        let res = match hopr::telemetry() {
                            Ok(t) => Some(t),
                            Err(err) => {
                                tracing::error!(?err, "failed to collect hopr telemetry");
                                None
                            }
                        };
                        let _ = resp.send(Response::Telemetry(res));
                    }

                    WorkerCommand::FundingTool(secret) => match self.phase.clone() {
                        Phase::CheckingSafe {
                            node_balance,
                            query_safe,
                            funding_tool,
                            deploy_safe_error,
                        } => match funding_tool {
                            balance::FundingTool::NotStarted | balance::FundingTool::CompletedError(_) => {
                                self.phase = Phase::CheckingSafe {
                                    node_balance,
                                    query_safe,
                                    funding_tool: balance::FundingTool::InProgress,
                                    deploy_safe_error,
                                };
                                self.spawn_funding_runner(secret, results_sender);
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::Started));
                            }
                            balance::FundingTool::InProgress => {
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::InProgress));
                            }
                            balance::FundingTool::CompletedSuccess => {
                                let _ = resp.send(Response::funding_tool(command::FundingToolResponse::Done));
                            }
                        },
                        _ => {
                            let _ = resp.send(Response::funding_tool(command::FundingToolResponse::WrongPhase));
                        }
                    },
                }
                true
            }
        }
    }

    /// Results are events from async runners
    #[tracing::instrument(skip(self, results_sender, results), level = "debug", ret)]
    async fn on_results(&mut self, results: Results, results_sender: &mpsc::Sender<Results>) -> bool {
        tracing::debug!(%results, phase = ?self.phase, "on runner results");
        match results {
            Results::IncentiveOperations { res } => {
                if !self.on_results_incentive_operations(res, results_sender).await {
                    return false;
                }
            }
            Results::IncentiveOperationsRetry { error } => {
                if matches!(self.phase, Phase::Initial { .. }) {
                    self.phase = Phase::Initial {
                        last_error: Some(error),
                    };
                }
            }
            Results::HoprConstruction(edgli_state) => {
                if matches!(self.phase, Phase::Starting { .. }) {
                    self.phase = Phase::Starting {
                        edgli_init_state: Some(edgli_state),
                        last_error: None,
                    };
                } else {
                    tracing::warn!(?self.phase, "hopr construction result received in unexpected phase");
                }
            }
            Results::MinimumBalanceRecommendation { res } => match res {
                Ok(rec) => {
                    tracing::info!(?rec, "received minimum balance recommendation");
                    self.minimum_balance_recommendation = Some(rec);
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch minimum balance recommendation - retrying");
                    self.spawn_minimum_balance_recommendation_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::IdealBalanceRecommendation { res } => match res {
                Ok(rec) => {
                    tracing::info!(?rec, "received ideal balance recommendation");
                    self.ideal_balance_recommendation = Some(rec);
                    self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to fetch ideal balance recommendation - retrying");
                    self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::CapacityAllocations { res } => match res {
                Ok(allocations) => {
                    tracing::info!(count = allocations.len(), "received capacity allocations");
                    let has_channels = allocations
                        .keys()
                        .any(|k| matches!(k, balance::CapacityAllocator::Peer(_)));
                    self.capacity_allocations = Some(allocations);
                    if has_channels && let Some(hopr) = self.hopr.clone() {
                        let dest_ids: Vec<String> = self.route_healths.keys().cloned().collect();
                        for id in &dest_ids {
                            if let (Some(rh), Some(dest)) =
                                (self.route_healths.get_mut(id), self.config.destinations.get(id))
                            {
                                rh.any_channel_available(&hopr, dest, &self.config.connection, results_sender);
                            }
                        }
                    }
                    let delay = if route_health::any_needs_channel(self.route_healths.values()) {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_secs(60)
                    };
                    self.spawn_capacity_allocations_runner(results_sender, delay);
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to fetch capacity allocations - retrying");
                    self.spawn_capacity_allocations_runner(results_sender, Duration::from_secs(10));
                }
            },
            Results::Balances { res } => match res {
                Ok(balances) => {
                    tracing::info!(%balances, "received balances from hopr");
                    self.balances = Some(balances);
                    self.spawn_balances_runner(results_sender, Duration::from_secs(60));
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch balances from hopr - retrying");
                    self.spawn_balances_runner(results_sender, Duration::from_secs(10));
                }
            },

            Results::NodeBalance { res } => self.on_results_node_balance(res, results_sender).await,
            Results::QuerySafe { res } => self.on_results_query_safe(res, results_sender).await,
            Results::DeploySafe { res } => self.on_results_deploy_safe(res, results_sender).await,
            Results::FundingTool { res } => self.on_results_funding_tool(res),

            Results::PersistSafe { res, safe_module } => match res {
                Ok(()) => {
                    tracing::info!("safe module persisted");
                }
                Err(err) => {
                    tracing::error!(?err, "failed to persist safe module - retrying");
                    self.spawn_store_safe(safe_module, results_sender, Duration::from_secs(10));
                }
            },

            Results::Hopr { res, safe_module } => match res {
                Ok(hopr) => {
                    tracing::info!("hopr runner started successfully");
                    self.phase = Phase::HoprSyncing;
                    self.hopr = Some(Arc::new(hopr));
                    self.spawn_node_wxhopr_withdraw_runner(results_sender, Duration::ZERO);
                    self.try_start_reactor(results_sender).await;
                    self.spawn_wait_for_running(results_sender, Duration::from_secs(1));
                }
                Err(err) => {
                    tracing::error!(?err, "hopr runner failed to start - trying again in 10 seconds");
                    self.retry_hopr_runner(err.to_string(), safe_module, results_sender, Duration::from_secs(10));
                }
            },

            // The runner retries indefinitely so res is always Ok; log just in case.
            Results::NodeWxhoprWithdraw { res } => {
                if let Err(err) = res {
                    tracing::error!(?err, "failed to withdraw node wxHOPR to safe");
                }
                self.spawn_node_wxhopr_withdraw_runner(results_sender, NODE_WXHOPR_WITHDRAW_INTERVAL);
            }

            Results::HoprRunning => {
                self.on_hopr_running(results_sender);
            }

            Results::ConnectedPeers {
                connected,
                announced_ips,
            } => match connected {
                Ok(peers) => {
                    tracing::info!(num_peers = %peers.len(), "fetched connected peers");
                    let all_peers = HashSet::from_iter(peers.iter().cloned());
                    let dest_ids: Vec<String> = self.route_healths.keys().cloned().collect();
                    let channels_already_available = self
                        .capacity_allocations
                        .as_ref()
                        .is_some_and(|map| map.keys().any(|k| matches!(k, balance::CapacityAllocator::Peer(_))));
                    for (idx, id) in dest_ids.into_iter().enumerate() {
                        if let Some(dest) = self.config.destinations.get(&id).cloned()
                            && let Some(rh) = self.route_healths.get_mut(&id)
                            && let Some(hopr) = self.hopr.clone()
                        {
                            let stagger = Duration::from_millis((idx as u64).saturating_mul(500));
                            rh.peers(
                                &all_peers,
                                &hopr,
                                &dest,
                                &self.config.connection,
                                results_sender,
                                stagger,
                            );
                            // If peers just moved this route into NeedsChannel and capacity
                            // allocations already show open channels, complete the transition
                            // immediately rather than waiting for the next capacity tick.
                            if channels_already_available && rh.needs_channel() {
                                rh.any_channel_available(&hopr, &dest, &self.config.connection, results_sender);
                            }
                        }
                    }

                    // Refresh the killswitch / routing-bypass allowlist while connected.
                    if matches!(self.phase, Phase::Connected(_)) {
                        let now = Instant::now();
                        for ip in &announced_ips {
                            self.peer_ip_last_seen.insert(*ip, now);
                        }
                        self.peer_ip_last_seen
                            .retain(|_, t| now.duration_since(*t) < Duration::from_secs(PEER_IP_HYSTERESIS_SECS));
                        let mut alive: Vec<net::Ipv4Addr> = self.peer_ip_last_seen.keys().copied().collect();
                        alive.sort_unstable();
                        if alive.is_empty() {
                            tracing::debug!("peer allowlist refresh skipped: live peer IP set is empty");
                        } else if alive != self.last_sent_peer_ips {
                            self.last_sent_peer_ips = alive.clone();
                            let request = RequestToRoot::UpdatePeerIps { peer_ips: alive };
                            let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                        }
                    }

                    let delay = if matches!(self.phase, Phase::Connected(_))
                        || route_health::any_needs_peers(self.route_healths.values())
                    {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_secs(90)
                    };
                    self.spawn_connected_peers(results_sender, delay);
                }
                Err(err) => {
                    tracing::error!(?err, "failed to fetch connected peers");
                    self.spawn_connected_peers(results_sender, Duration::from_secs(10));
                }
            },

            Results::ConnectionEvent(evt) => {
                tracing::debug!(%evt, "handling connection runner event");
                match self.phase.clone() {
                    Phase::Connecting(mut conn) => match evt {
                        connection::up::Event::Progress(e) => {
                            if let connection::up::Progress::GenerateWg(blokli_ips) = e.as_ref() {
                                self.cached_resolved_blokli_ips = blokli_ips.clone();
                                let request = RequestToRoot::CacheBlokliIps {
                                    ips: blokli_ips.clone(),
                                };
                                let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                            }
                            conn.connect_progress(e);
                            self.phase = Phase::Connecting(conn);
                        }
                        connection::up::Event::Setback(e) => {
                            if let Some(rh) = self.route_healths.get_mut(&conn.destination.id) {
                                rh.with_error(e.to_string());
                            }
                        }
                    },
                    phase => {
                        tracing::warn!(%evt, ?phase, "received connection event in unexpected phase");
                    }
                }
            }

            Results::DisconnectionEvent { wg_public_key, evt } => {
                tracing::debug!(%wg_public_key, %evt, "handling disconnection runner event");
                if let Some(conn) = self
                    .ongoing_disconnections
                    .iter_mut()
                    .find(|c| c.wg_public_key == wg_public_key)
                {
                    conn.disconnect_evt(evt);
                } else {
                    tracing::warn!(%evt, ?self.phase, "received disconnection event for unknown connection");
                }
                // potentially reconnect early after wg disconnected
                // this might only happen when reconnecting to a different destination after a
                // connection established successfully
                if matches!(evt, connection::down::Event::OpenBridge) {
                    self.act_on_target(results_sender);
                }
            }

            Results::ConnectionResult { res } => match (res, self.phase.clone()) {
                (Ok(session), Phase::Connecting(mut conn)) => {
                    tracing::info!(%conn, "connection established successfully");
                    conn.connected();
                    self.phase = Phase::Connected(conn.clone());
                    self.pseudonym_cache.remove(&conn.destination);
                    let route = format!(
                        "{}({})",
                        conn.destination.pretty_print_path(),
                        log_output::address(&conn.destination.address)
                    );
                    log_output::print_session_established(route.as_str());
                    self.spawn_session_monitoring(session, results_sender);
                    self.spawn_tunnel_ping_probe(results_sender);
                }
                (Ok(_), phase) => {
                    tracing::warn!(?phase, "unawaited connection established successfully");
                }
                (Err(err), Phase::Connecting(conn)) => {
                    tracing::error!(?err, %conn, "connection failed");
                    if let Some(rh) = self.route_healths.get_mut(&conn.destination.id) {
                        rh.with_error(err.to_string());
                    }
                    if let Some(dest) = self.target_destination.clone()
                        && dest == conn.destination
                    {
                        tracing::info!(%dest, "restarting connection worker process due to final connection error");
                        return false;
                    }
                }
                (Err(err), phase) => {
                    tracing::warn!(%err, ?phase, "connection failed in unexpecting state");
                }
            },

            Results::DisconnectionResult { wg_public_key, res } => {
                match res {
                    Ok(_) => {
                        tracing::info!(%wg_public_key, "disconnected successful");
                    }
                    Err(err) => {
                        tracing::error!(?err, %wg_public_key, "disconnection failed");
                    }
                }
                self.ongoing_disconnections.retain(|c| c.wg_public_key != wg_public_key);
                self.act_on_target(results_sender);
            }

            Results::SessionMonitorFailed => match self.phase.clone() {
                Phase::Connected(conn) => {
                    tracing::warn!(%conn, "session monitor failed - reconnecting");
                    self.disconnect_from_connection(&conn, results_sender);
                }
                phase => {
                    tracing::error!(?phase, "session monitor failed in unexpected phase");
                }
            },

            Results::TunnelPingResult { rtt } => {
                if let Phase::Connected(conn) = self.phase.clone()
                    && let Some(rh) = self.route_healths.get_mut(&conn.destination.id)
                {
                    let failures = rh.tunnel_ping_result(rtt);
                    let max = self.config.connection.health_check_intervals.tunnel_ping_max_failures;
                    if failures >= max {
                        tracing::warn!(%conn, failures, "tunnel ping exceeded max failures - reconnecting");
                        self.disconnect_from_connection(&conn, results_sender);
                    }
                }
            }

            Results::ConnectionRequestToRoot(respondable_request) => match respondable_request {
                RunnerToRoot::KillswitchLockdown {
                    peer_ips,
                    interface,
                    resp,
                } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Unit(resp));
                    let request = RequestToRoot::KillswitchLockdown {
                        request_id,
                        peer_ips,
                        interface,
                    };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::DynamicWgRouting { wg_data, resp } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Str(resp));
                    let request = RequestToRoot::DynamicWgRouting { request_id, wg_data };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::StaticWgRouting {
                    wg_data,
                    peer_ips,
                    resp,
                } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Str(resp));
                    let request = RequestToRoot::StaticWgRouting {
                        request_id,
                        wg_data,
                        peer_ips,
                    };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::Ping { options, resp } => {
                    let request_id = self.next_request_id();
                    self.responders.insert(request_id, Responder::Duration(resp));
                    let request = RequestToRoot::Ping { request_id, options };
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }

                RunnerToRoot::TearDownWg => {
                    let request = RequestToRoot::TearDownWg;
                    let _ = self.outgoing_sender.send(CoreToWorker::RequestToRoot(request)).await;
                }
            },

            Results::HealthCheck { id, outcome } => {
                tracing::info!(%id, ?outcome, "received health check");
                if let Some(dest) = self.config.destinations.get(&id).cloned()
                    && let Some(rh) = self.route_healths.get_mut(&id)
                {
                    let was_ready = rh.is_ready_to_connect();
                    rh.health_check_result(
                        outcome,
                        self.hopr.as_ref().unwrap(),
                        &dest,
                        &self.config.connection,
                        results_sender,
                    );
                    // Trigger connection if we just became ready
                    if !was_ready && rh.is_ready_to_connect() {
                        self.act_on_target(results_sender);
                    }
                }
            }

            Results::RetryReactor => {
                self.try_start_reactor(results_sender).await;
            }

            Results::NerdStatsTicketStats {
                res: ticket_stats_status,
                resp,
            } => match &self.phase {
                Phase::Connecting(conn) => {
                    let conn_stats = command::ConnStats::from_conn(conn, self.node_address);
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse::Connecting(
                        ticket_stats_status,
                        conn_stats,
                    )));
                }
                Phase::Connected(conn) => {
                    let conn_stats = command::ConnStats::from_conn(conn, self.node_address);
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse::Connected(
                        ticket_stats_status,
                        conn_stats,
                    )));
                }
                _ => {
                    let _ = resp.send(Response::nerd_stats(command::NerdStatsResponse::NoInfo(
                        ticket_stats_status,
                    )));
                }
            },
        };
        return true;
    }

    // Returns false to signal core exit, which lets root restart the worker.
    async fn on_results_incentive_operations(
        &mut self,
        res: Result<Arc<dyn IncentiveOperations>, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) -> bool {
        match res {
            Ok(incentive_operations) => {
                tracing::info!("incentive operations handle created successfully");
                self.incentive_operations = Some(incentive_operations);
                self.spawn_minimum_balance_recommendation_runner(results_sender, Duration::ZERO);
                self.determine_next_phase_from_safe_disk_query(results_sender).await;
                true
            }
            Err(err) => {
                tracing::error!(
                    ?err,
                    "failed to create incentive operations handle after all retries - restarting core"
                );
                false
            }
        }
    }

    async fn on_results_node_balance(
        &mut self,
        res: Result<balance::PreSafe, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (
                Ok(presafe),
                Phase::CheckingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::info!(%presafe, "on presafe node balance");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance: _,
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::error!(?err, "failed to fetch presafe node balance - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Error(err.to_string()),
                    query_safe,
                    deploy_safe_error,
                    funding_tool,
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring presafe node balance result in unexpected phase");
            }
        }
    }

    async fn on_results_query_safe(
        &mut self,
        res: Result<Option<SafeModule>, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (Ok(Some(safe_module)), Phase::CheckingSafe { .. }) => {
                tracing::info!(?safe_module, "found safe module");
                self.cancel_presafe_queries.cancel();
                self.cancel_presafe_queries = self.cancel_on_shutdown.child_token();
                // start edge client with queried safe module
                self.start_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
                // try persisting safe module to disk - might fail but we consider this non critical
                self.spawn_store_safe(safe_module, results_sender, Duration::ZERO);
            }
            (
                Ok(None),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::info!("found no deployed safe module");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Success(None),
                    deploy_safe_error,
                    funding_tool,
                };
                // trigger retry - will be canceled if safe deployment starts
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
                self.trigger_deploy_safe(results_sender);
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe: _,
                    deploy_safe_error,
                    funding_tool,
                },
            ) => {
                tracing::error!(?err, "failed to query safe module - retrying");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe: Querying::Error(err.to_string()),
                    deploy_safe_error,
                    funding_tool,
                };
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring query safe result in unexpected phase");
            }
        }
    }

    async fn on_results_deploy_safe(
        &mut self,
        res: Result<SafeModule, runner::Error>,
        results_sender: &mpsc::Sender<Results>,
    ) {
        match (res, self.phase.clone()) {
            (Ok(safe_module), Phase::DeployingSafe { .. }) => {
                tracing::info!(?safe_module, "deployed safe module");
                // start edge client with new safe module
                self.start_hopr_runner(safe_module.clone(), results_sender, Duration::ZERO);
                // try persisting safe module to disk - might fail but we consider this non critical
                self.spawn_store_safe(safe_module, results_sender, Duration::ZERO);
            }
            (
                Err(err),
                Phase::DeployingSafe {
                    node_balance,
                    query_safe,
                },
            ) => {
                tracing::error!(?err, "failed to deploy safe module - retrying from balance check");
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error: Some(err.to_string()),
                    funding_tool: balance::FundingTool::NotStarted,
                };
                self.spawn_node_balance_runner(results_sender, Duration::from_secs(10));
                self.spawn_query_safe_runner(results_sender, Duration::from_secs(10));
            }
            (res, phase) => {
                tracing::warn!(?res, ?phase, "ignoring deploy safe result in unexpected phase");
            }
        }
    }

    fn on_results_funding_tool(&mut self, res: Result<Option<String>, runner::Error>) {
        match (res, self.phase.clone()) {
            (
                Ok(None),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedSuccess,
                }
            }
            (
                Ok(Some(reason)),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedError(reason),
                }
            }
            (
                Err(err),
                Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    ..
                },
            ) => {
                self.phase = Phase::CheckingSafe {
                    node_balance,
                    query_safe,
                    deploy_safe_error,
                    funding_tool: balance::FundingTool::CompletedError(err.to_string()),
                }
            }

            (res, phase) => {
                tracing::warn!(?res, ?phase, "unexpected funding tool response in wrong phase");
            }
        }
    }

    fn trigger_deploy_safe(&mut self, results_sender: &mpsc::Sender<Results>) {
        if let Phase::CheckingSafe {
            node_balance: Querying::Success(presafe),
            query_safe: Querying::Success(None),
            deploy_safe_error: _,
            funding_tool: _,
        } = self.phase.clone()
        {
            if presafe.node_xdai.is_zero() || presafe.node_wxhopr.is_zero() {
                tracing::warn!("insufficient funds to start safe deployment - waiting for funding");
            } else {
                self.phase = Phase::DeployingSafe {
                    node_balance: Querying::Success(presafe.clone()),
                    query_safe: Querying::Success(None),
                };
                self.cancel_presafe_queries.cancel();
                self.cancel_presafe_queries = self.cancel_on_shutdown.child_token();
                self.spawn_safe_deployment_runner(&presafe, results_sender);
            }
        }
    }

    fn spawn_initial_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::create_incentive_operations(&worker_params, blokli_config.into(), results_sender).await;
                })
                .await
        });
    }

    async fn determine_next_phase_from_safe_disk_query(&mut self, results_sender: &mpsc::Sender<Results>) {
        let res = hopr_config::read_safe(self.worker_params.state_home()).await;
        match res {
            Ok(safe_module) => {
                tracing::debug!(?safe_module, "found existing safe module - starting hopr runner");
                // start edge client with existing safe module
                self.start_hopr_runner(safe_module, results_sender, Duration::ZERO);
            }
            Err(err) => {
                if matches!(err, hopr_config::Error::NoFile) {
                    tracing::info!("no persisted safe module found - querying incentive operations");
                } else {
                    tracing::warn!(
                        ?err,
                        "error deserializing existing safe module - querying incentive operations"
                    );
                }
                self.phase = Phase::CheckingSafe {
                    node_balance: Querying::Init,
                    query_safe: Querying::Init,
                    deploy_safe_error: None,
                    funding_tool: balance::FundingTool::NotStarted,
                };
                self.spawn_query_safe_runner(results_sender, Duration::ZERO);
                self.spawn_node_balance_runner(results_sender, Duration::ZERO);
            }
        }
    }

    fn spawn_query_safe_runner(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::query_safe(incentive_operations, results_sender).await
                    })
                    .await
            });
        }
    }

    fn spawn_node_balance_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_presafe_queries.clone();
        let results_sender = results_sender.clone();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::node_balance(incentive_operations, results_sender).await
                    })
                    .await
            });
        }
    }

    fn spawn_funding_runner(&self, secret: String, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move { runner::funding_tool(worker_params, secret, results_sender).await })
                .await;
        });
    }

    fn spawn_safe_deployment_runner(&self, presafe: &balance::PreSafe, results_sender: &mpsc::Sender<Results>) {
        let cancel = self.cancel_on_shutdown.clone();
        let presafe = presafe.clone();
        let results_sender = results_sender.clone();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner::safe_deployment(incentive_operations, presafe, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_store_safe(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let state_home = self.worker_params.state_home();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::persist_safe(state_home, safe_module, results_sender).await;
                })
                .await
        });
    }

    fn spawn_hopr_runner(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let worker_params = self.worker_params.clone();
        let blokli_config = self.config.blokli.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    runner::hopr(worker_params, blokli_config, &safe_module, results_sender).await;
                })
                .await
        });
    }

    fn start_hopr_runner(&mut self, safe_module: SafeModule, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        self.phase = Phase::Starting {
            edgli_init_state: None,
            last_error: None,
        };
        self.spawn_hopr_runner(safe_module, results_sender, delay);
    }

    fn retry_hopr_runner(
        &mut self,
        error: String,
        safe_module: SafeModule,
        results_sender: &mpsc::Sender<Results>,
        delay: Duration,
    ) {
        self.phase = Phase::Starting {
            edgli_init_state: None,
            last_error: Some(error),
        };
        self.spawn_hopr_runner(safe_module, results_sender, delay);
    }

    fn spawn_minimum_balance_recommendation_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        let cfg = self.config.strategy.clone().into();
        if let Some(incentive_operations) = self.incentive_operations.clone() {
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::minimum_balance_recommendation(incentive_operations, cfg, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_ideal_balance_recommendation_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let cfg = self.config.strategy.clone().into();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::ideal_balance_recommendation(hopr, cfg, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_capacity_allocations_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::capacity_allocations(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_balances_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::balances(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_node_wxhopr_withdraw_runner(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let (Some(ops), Some(hopr)) = (self.incentive_operations.clone(), self.hopr.clone()) {
            let safe_address = hopr.info().safe_address;
            let cancel = self.cancel_node_wxhopr.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::node_wxhopr_withdraw(ops, safe_address, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_wait_for_running(&mut self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::wait_for_running(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connected_peers(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        time::sleep(delay).await;
                        runner::connected_peers(hopr, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_connection_runner(
        &mut self,
        destination: Destination,
        exit: route_health::ExitHealth,
        results_sender: &mpsc::Sender<Results>,
    ) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let conn = connection::up::Up::new(destination.clone());
            let config_connection = self.config.connection.clone();
            let config_wireguard = self.config.wireguard.clone();
            let hopr = hopr.clone();
            // Entry is kept until connection is confirmed so retries within the TTL can reuse it.
            let cached_pseudonym = self.pseudonym_cache.get(&destination);
            if let Some(pseudonym) = &cached_pseudonym {
                tracing::info!(%destination, %pseudonym, "reusing cached session pseudonym for reconnection");
            }
            let runner = connection::up::runner::Runner::new(
                conn.destination.clone(),
                config_connection,
                config_wireguard,
                hopr,
                self.worker_params.clone(),
                self.cached_resolved_blokli_ips.clone(),
                cached_pseudonym,
            );
            let results_sender = results_sender.clone();
            if let Some(rh) = self.route_healths.get_mut(&destination.id) {
                rh.connecting(
                    self.hopr.as_ref().unwrap(),
                    &destination,
                    exit,
                    &self.config.connection,
                    &results_sender,
                );
            }
            self.phase = Phase::Connecting(conn);
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner.start(results_sender).await;
                    })
                    .await;
            });
        }
    }

    fn spawn_disconnection_runner(&mut self, disconn: &connection::down::Down, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_on_shutdown.clone();
            let config_connection = self.config.connection.clone();
            let hopr = hopr.clone();
            let runner = connection::down::runner::Runner::new(disconn.clone(), hopr, config_connection);
            let results_sender = results_sender.clone();
            self.ongoing_disconnections.push(disconn.clone());
            let outgoing_sender = self.outgoing_sender.clone();
            tokio::spawn(async move {
                // this is a oneshot command and we do not wait for any result
                let _ = outgoing_sender
                    .send(CoreToWorker::RequestToRoot(RequestToRoot::TearDownWg))
                    .await;
                cancel
                    .run_until_cancelled(async move {
                        runner.start(results_sender).await;
                    })
                    .await;
            });
        }
    }

    fn spawn_session_monitoring(&self, session: SessionClientMetadata, results_sender: &mpsc::Sender<Results>) {
        if let Some(hopr) = self.hopr.clone() {
            let cancel = self.cancel_connection.clone();
            let results_sender = results_sender.clone();
            tokio::spawn(async move {
                cancel
                    .run_until_cancelled(async move {
                        runner::monitor_session(hopr, &session, results_sender).await;
                    })
                    .await
            });
        }
    }

    fn spawn_tunnel_ping_probe(&self, results_sender: &mpsc::Sender<Results>) {
        let interval = self.config.connection.health_check_intervals.tunnel_ping;
        let cancel = self.cancel_connection.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    runner::tunnel_ping_loop(interval, results_sender).await;
                })
                .await
        });
    }

    #[tracing::instrument(skip(self, results_sender), level = "debug", ret)]
    fn act_on_target(&mut self, results_sender: &mpsc::Sender<Results>) {
        tracing::debug!(target = ?self.target_destination, phase = ?self.phase, "acting on target destination");
        match (self.target_destination.clone(), self.phase.clone()) {
            // Connecting from ready
            (Some(dest), Phase::HoprRunning) => {
                if let Some(rh) = self.route_healths.get(&dest.id) {
                    if let Some(exit) = rh.ready_to_connect() {
                        tracing::info!(destination = %dest, "establishing connection to new destination");
                        self.spawn_connection_runner(dest.clone(), exit, results_sender);
                    } else if rh.is_unrecoverable() {
                        tracing::error!(destination = %dest,route_health = ?rh.state(),  "refusing connection because of route health");
                    } else {
                        tracing::warn!(destination = %dest,route_health = ?rh.state(),  "waiting for better route health before connecting");
                    }
                } else {
                    tracing::warn!(destination = %dest, "refusing connection: destination has no route health tracker");
                }
            }
            // Connecting to different destination while already connected
            (Some(dest), Phase::Connected(conn)) if dest != conn.destination => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while connected");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Connecting to different destination while already connecting
            (Some(dest), Phase::Connecting(conn)) if dest != conn.destination => {
                tracing::info!(current = %conn.destination, new = %dest, "connecting to different destination while already connecting");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Disconnecting from established connection
            (None, Phase::Connected(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from destination");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // Disconnecting while establishing connection
            (None, Phase::Connecting(conn)) => {
                tracing::info!(current = %conn.destination, "disconnecting from ongoing connection attempt");
                self.disconnect_from_connection(&conn, results_sender);
            }
            // No action needed
            _ => {}
        }
    }

    fn disconnect_from_connection(&mut self, conn: &connection::up::Up, results_sender: &mpsc::Sender<Results>) {
        // Cache the pseudonym so a reconnect within the TTL window can reuse exit node SURBs.
        if let Some((_, session)) = &conn.active_session
            && let Some(client_id) = session.active_clients.first()
            && let Ok(session_id) = SessionId::from_str(client_id)
        {
            self.pseudonym_cache.insert(&conn.destination, *session_id.pseudonym());
        }
        self.cancel_connection.cancel();
        self.cancel_connection = self.cancel_on_shutdown.child_token();
        self.responders.clear();
        self.phase = Phase::HoprRunning;
        // Clear hysteresis state so stale IPs from this session don't pollute the next.
        self.peer_ip_last_seen.clear();
        if let Some(dest) = self.config.destinations.get(&conn.destination.id).cloned()
            && let Some(rh) = self.route_healths.get_mut(&conn.destination.id)
        {
            rh.disconnecting(
                self.hopr.as_ref().unwrap(),
                &dest,
                &self.config.connection,
                results_sender,
            );
        }
        if let Ok(disconn) = conn.try_into() {
            self.spawn_disconnection_runner(&disconn, results_sender);
        } else {
            // connection did not even generate a wg pub key - so we can immediately try to connect again
            self.act_on_target(results_sender);
        }
    }

    fn on_hopr_running(&mut self, results_sender: &mpsc::Sender<Results>) {
        self.phase = Phase::HoprRunning;
        self.spawn_ideal_balance_recommendation_runner(results_sender, Duration::ZERO);
        self.spawn_capacity_allocations_runner(results_sender, Duration::ZERO);
        self.spawn_balances_runner(results_sender, Duration::ZERO);
        if route_health::any_needs_peers(self.route_healths.values()) {
            self.spawn_connected_peers(results_sender, Duration::ZERO);
        } else {
            self.act_on_target(results_sender);
        }
    }

    async fn try_start_reactor(&mut self, results_sender: &mpsc::Sender<Results>) {
        if self.strategy_handle.is_some() {
            return;
        }
        let Some(edgli) = self.hopr.as_ref() else { return };
        match edgli.start_telemetry_reactor(self.config.strategy.clone().into()).await {
            Ok(strategy_process) => {
                tracing::info!("started edge node telemetry reactor");
                self.strategy_handle = Some(strategy_process);
            }
            Err(err) => {
                tracing::error!(?err, "failed to start edge node telemetry reactor - retrying in 10s");
                self.spawn_retry_reactor(results_sender, Duration::from_secs(10));
            }
        }
    }

    fn spawn_retry_reactor(&self, results_sender: &mpsc::Sender<Results>, delay: Duration) {
        let cancel = self.cancel_on_shutdown.clone();
        let results_sender = results_sender.clone();
        tokio::spawn(async move {
            cancel
                .run_until_cancelled(async move {
                    time::sleep(delay).await;
                    let _ = results_sender.send(Results::RetryReactor).await;
                })
                .await
        });
    }
}
