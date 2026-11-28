use gnosis_vpn_ipc::{
    self as ipc, Address, Balance, Balances, CallError, ConnectResponse, Connection, Destination, DestinationId,
    DestinationSource, DestinationStatus, DisconnectResponse, FundingLevel, PixChunkStats, PixPhase, QuickProbeCheck,
    QuickProbeResponse, QuickProbeState, Request, Response, RouteHealthState, RunMode, StartClientResponse, Status,
    StopClientResponse,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};
use tracing::{debug, error, info, warn};

use crate::fixtures::lib::{self, ConditionCheck};

const STRONG_DESTINATIONS: usize = 3;
const STRONG_PERCENT_OF_BEST: u8 = 60;

/// Thin wrapper around the gnosis_vpn control socket used during system tests.
pub struct ControlClient {
    socket_path: PathBuf,
    ctl_binary: PathBuf,
    worker_user: uzers::User,
}

impl ControlClient {
    /// Creates a new client bound to a Unix domain socket path.
    pub fn new(socket_path: PathBuf, ctl_binary: PathBuf, worker_user: &str) -> anyhow::Result<Self> {
        let worker_user = uzers::get_user_by_name(worker_user)
            .ok_or_else(|| anyhow::anyhow!("worker user {worker_user} not found"))?;
        Ok(Self {
            socket_path,
            ctl_binary,
            worker_user,
        })
    }

    /// Sends a raw request to the control socket and returns the daemon response.
    pub async fn send(&self, request: Request) -> anyhow::Result<Response> {
        let socket_path = self.socket_path.clone();
        let logged = request.clone();
        let result = tokio::task::spawn_blocking(move || ipc::call(&socket_path, request)).await?;
        match result {
            Ok(resp) => Ok(resp),
            Err(CallError::ServiceNotRunning) => {
                error!(request = ?logged, "service not running when sending request");
                Err(CallError::ServiceNotRunning.into())
            }
            Err(error) => {
                error!(%error, request = ?logged, "error while sending request");
                Err(error.into())
            }
        }
    }

    /// Verifies the daemon responds to ping requests.
    pub async fn ping(&self) -> anyhow::Result<()> {
        self.send(Request::Ping).await.and_then(|result| {
            matches!(result, Response::Pong)
                .then_some(())
                .ok_or(anyhow::anyhow!("unexpected ping response {result:?}"))
        })
    }

    /// Fetches current daemon status information.
    pub async fn status(&self) -> anyhow::Result<Option<Status>> {
        match self.send(Request::Status).await {
            Ok(Response::Status(status)) => Ok(Some(*status)),
            Ok(resp) => Err(anyhow::anyhow!("unexpected status response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Retrieves the node and safe balances.
    pub async fn balance(&self) -> anyhow::Result<Balances> {
        match self.send(Request::Balance).await {
            Ok(Response::Balance(ipc::BalanceResponse::Available(balances))) => Ok(*balances),
            Ok(Response::Balance(ipc::BalanceResponse::Unavailable { reason })) => {
                Err(anyhow::anyhow!("balance error: {reason}"))
            }
            Ok(resp) => Err(anyhow::anyhow!("unexpected balance response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Initiates a VPN connection to the provided destination.
    pub async fn connect(&self, destination: DestinationId) -> anyhow::Result<ConnectResponse> {
        match self.send(Request::Connect { destination }).await {
            Ok(Response::Connect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected connect response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Close the VPN connection.
    pub async fn disconnect(&self) -> anyhow::Result<DisconnectResponse> {
        match self.send(Request::Disconnect).await {
            Ok(Response::Disconnect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected disconnect response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Start the client as the worker user: the worker runs as whoever asks, and root may not own one.
    pub async fn start(&self) -> anyhow::Result<StartClientResponse> {
        let output = tokio::process::Command::new(&self.ctl_binary)
            .arg("--socket-path")
            .arg(&self.socket_path)
            .args(["--output", "json", "start-client", "10m"])
            .uid(self.worker_user.uid())
            .gid(self.worker_user.primary_group_id())
            .output()
            .await?;
        match serde_json::from_slice(&output.stdout) {
            Ok(Response::StartClient(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected start response {resp:?}")),
            Err(error) => Err(anyhow::anyhow!(
                "unreadable start response ({}): {error}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )),
        }
    }

    /// Stop the client
    pub async fn stop(&self) -> anyhow::Result<StopClientResponse> {
        match self.send(Request::StopClient).await {
            Ok(Response::StopClient(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected stop response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Waits until the control API responds to ping requests.
    pub async fn wait_for_service_running(&self, timeout: Duration) -> anyhow::Result<()> {
        lib::wait_for_condition("service running", timeout, Duration::from_secs(2), || async {
            match self.ping().await {
                Ok(_) => {
                    info!("gnosis_vpn service is pingable");
                    Ok(ConditionCheck::Ready(()))
                }
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
    }

    /// Waits until a safe is created and available.
    pub async fn wait_for_safe_created(&self, timeout: Duration) -> anyhow::Result<()> {
        lib::wait_for_condition("safe created", timeout, Duration::from_secs(5), || async {
            match self.status().await {
                Ok(Some(status)) => match status.run_mode {
                    RunMode::Init { .. } | RunMode::NotRunning | RunMode::Restarting => Ok(ConditionCheck::Pending),
                    RunMode::PreparingSafe { .. } => {
                        warn!("safe being prepared");
                        Ok(ConditionCheck::Pending)
                    }
                    RunMode::DeployingSafe { .. }
                    | RunMode::Warmup { .. }
                    | RunMode::Running { .. }
                    | RunMode::Shutdown => {
                        info!("safe is created and ready");
                        Ok(ConditionCheck::Ready(()))
                    }
                },
                Ok(None) => Ok(ConditionCheck::Pending),
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
    }

    /// Waits for the worker's funding figures, then fails at once if they can't pay for a run.
    pub async fn check_funding(&self, timeout: Duration) -> anyhow::Result<()> {
        let balances = lib::wait_for_condition("funding figures", timeout, Duration::from_secs(5), || async {
            match self.balance().await {
                Ok(balances) if balances.ideal_balance.is_some() && balances.funding_status.is_some() => {
                    Ok(ConditionCheck::Ready(balances))
                }
                Ok(_) => Ok(ConditionCheck::Pending),
                Err(error) => {
                    debug!(%error, "balances not available yet");
                    Ok(ConditionCheck::Pending)
                }
            }
        })
        .await?;
        ensure_run_funded(&balances)
    }

    /// Waits until the node reports a running state.
    pub async fn wait_for_node_running(&self, timeout: Duration) -> anyhow::Result<()> {
        lib::wait_for_condition("node running", timeout, Duration::from_secs(10), || async {
            match self.status().await {
                Ok(Some(status)) => {
                    if matches!(status.run_mode, RunMode::Running { .. }) {
                        info!("node is in Running state");
                        Ok(ConditionCheck::Ready(()))
                    } else {
                        debug!("node not in Running state yet");
                        Ok(ConditionCheck::Pending)
                    }
                }
                Ok(None) => Ok(ConditionCheck::Pending),
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
    }

    /// Lets signals settle for `settle`, or less once enough destinations are close to the best.
    pub async fn wait_for_ready_destinations(
        &self,
        settle: Duration,
        timeout: Duration,
    ) -> anyhow::Result<DestinationReadiness> {
        let start = Instant::now();
        lib::wait_for_condition(
            "node ready to reach destinations",
            timeout,
            Duration::from_secs(10),
            || async {
                let Ok(Some(status)) = self.status().await else {
                    return Ok(ConditionCheck::Pending);
                };
                let readiness = DestinationReadiness::from_states(status.destinations);
                let strong = readiness
                    .ready()
                    .iter()
                    .filter(|ranked| ranked.percent_of_best >= STRONG_PERCENT_OF_BEST)
                    .count();

                if strong >= STRONG_DESTINATIONS {
                    info!(strong, "enough destinations close to the best");
                    return Ok(ConditionCheck::Ready(readiness));
                }

                let settled = start.elapsed() >= settle;
                if settled && !readiness.ready().is_empty() {
                    info!(ready = readiness.ready().len(), "signals settled");
                    return Ok(ConditionCheck::Ready(readiness));
                }

                debug!(
                    ready = readiness.ready().len(),
                    not_ready = readiness.not_ready().len(),
                    strong,
                    "waiting for signals to settle"
                );
                Ok(ConditionCheck::PendingWithValue(readiness))
            },
        )
        .await
    }

    /// Ensures a specific destination reaches the Connected state.
    pub async fn wait_for_connection_established(
        &self,
        destination: &Destination,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        lib::wait_for_condition("connection settlement", timeout, Duration::from_secs(5), || async {
            match self.status().await {
                Ok(Some(status)) => {
                    let dest_id = &destination.id;
                    let is_ours = status.connection.destination() == Some(dest_id);
                    let is_connected = is_ours && matches!(status.connection, Connection::Connected { .. });
                    let connecting_phase = match &status.connection {
                        Connection::Connecting { phase, .. } if is_ours => Some(phase),
                        _ => None,
                    };

                    if is_connected {
                        info!(%dest_id, "connection established successfully");
                        Ok(ConditionCheck::Ready(()))
                    } else if let Some(phase) = connecting_phase {
                        warn!(?phase, %dest_id, "connection is being established");
                        Ok(ConditionCheck::Pending)
                    } else {
                        warn!(%dest_id, "connection state is unknown");
                        Ok(ConditionCheck::Pending)
                    }
                }
                Ok(None) => Ok(ConditionCheck::Pending),
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
    }

    /// Runs one quick probe against the destination and returns its result.
    pub async fn quick_probe(&self, destination: &Destination, timeout: Duration) -> anyhow::Result<QuickProbeCheck> {
        let requested_at = SystemTime::now();
        let request = Request::QuickProbe {
            destination: destination.id.clone(),
        };
        match self.send(request).await? {
            Response::QuickProbe(QuickProbeResponse::Checking { .. } | QuickProbeResponse::AlreadyChecking { .. }) => {}
            response => return Err(anyhow::anyhow!("quick probe refused: {response:?}")),
        }

        lib::wait_for_condition("quick probe", timeout, Duration::from_secs(1), || async {
            let Ok(Some(status)) = self.status().await else {
                return Ok(ConditionCheck::Pending);
            };
            let probe = status
                .destinations
                .into_iter()
                .find(|state| state.destination.id == destination.id)
                .and_then(|state| state.route_health)
                .and_then(|health| health.quick_probe);
            // Results from before this request belong to an earlier probe
            match probe {
                Some(QuickProbeState::Checked(check)) if check.checked_at >= requested_at => {
                    Ok(ConditionCheck::Ready(check))
                }
                Some(QuickProbeState::Failed { checked_at, error }) if checked_at >= requested_at => {
                    Err(anyhow::anyhow!("quick probe failed: {error}"))
                }
                _ => Ok(ConditionCheck::Pending),
            }
        })
        .await
    }

    /// Polls until the destination's session paid for every chunk the exit asked for; on timeout returns the last snapshot.
    pub async fn wait_for_pix_funded(
        &self,
        destination: &Destination,
        timeout: Duration,
    ) -> anyhow::Result<Option<PixChunkStats>> {
        lib::wait_for_condition("pix funded", timeout, Duration::from_secs(5), || async {
            let Ok(Some(status)) = self.status().await else {
                return Ok(ConditionCheck::Pending);
            };
            let pix = match status.connection {
                Connection::Connecting {
                    destination: dest, pix, ..
                }
                | Connection::Connected {
                    destination: dest, pix, ..
                } if dest == destination.id => pix,
                _ => None,
            };
            let Some(stats) = pix else {
                warn!(dest_id = %destination.id, "no pix session yet");
                return Ok(ConditionCheck::PendingWithValue(None));
            };
            match stats.phase {
                PixPhase::Funded => Ok(ConditionCheck::Ready(Some(stats))),
                // A failed deposit never recovers within its session
                PixPhase::PaymentFailed => Err(anyhow::anyhow!("pix payment failed: {stats:?}")),
                PixPhase::Free | PixPhase::PaymentPending => {
                    warn!(
                        phase = ?stats.phase,
                        committed = stats.chunks_committed,
                        paid = stats.chunks_paid,
                        "waiting for pix chunks to be paid"
                    );
                    Ok(ConditionCheck::PendingWithValue(Some(stats)))
                }
            }
        })
        .await
    }

    /// Ensures there is no active VPN connection.
    pub async fn wait_for_disconnection(&self, timeout: Duration) -> anyhow::Result<()> {
        lib::wait_for_condition("disconnection", timeout, Duration::from_secs(2), || async {
            match self.disconnect().await {
                Ok(response) => match response {
                    DisconnectResponse::Disconnecting { destination: address } => {
                        info!("disconnecting from destination {address}");
                        Ok(ConditionCheck::Pending)
                    }
                    DisconnectResponse::NotConnected => {
                        info!("successfully disconnected");
                        Ok(ConditionCheck::Ready(()))
                    }
                },
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
    }

    /// Polls until every address is a discovered destination; on timeout returns the last snapshot, one entry per address.
    pub async fn wait_for_discovered_exits(
        &self,
        addresses: &[Address],
        timeout: Duration,
    ) -> anyhow::Result<Vec<Option<Destination>>> {
        lib::wait_for_condition("exit discovery", timeout, Duration::from_secs(10), || async {
            let Ok(Some(status)) = self.status().await else {
                return Ok(ConditionCheck::Pending);
            };
            let exits = discovered_exits(&status, addresses);
            let missing = exits.iter().filter(|exit| exit.is_none()).count();
            if missing == 0 {
                return Ok(ConditionCheck::Ready(exits));
            }
            warn!(
                missing,
                expected = addresses.len(),
                "waiting for exits to be discovered"
            );
            Ok(ConditionCheck::PendingWithValue(exits))
        })
        .await
    }

    /// Polls until every discovered address has a country; on timeout returns the last snapshot, one entry per address.
    pub async fn wait_for_exit_countries(
        &self,
        addresses: &[Address],
        timeout: Duration,
    ) -> anyhow::Result<Vec<Option<String>>> {
        lib::wait_for_condition("exit geolocation", timeout, Duration::from_secs(5), || async {
            let Ok(Some(status)) = self.status().await else {
                return Ok(ConditionCheck::Pending);
            };
            let countries = discovered_exits(&status, addresses)
                .into_iter()
                .map(|exit| exit.and_then(|dest| dest.geo).map(|geo| geo.country.to_string()))
                .collect::<Vec<_>>();
            let unresolved = countries.iter().filter(|country| country.is_none()).count();
            if unresolved == 0 {
                return Ok(ConditionCheck::Ready(countries));
            }
            warn!(unresolved, expected = addresses.len(), "waiting for exit countries");
            Ok(ConditionCheck::PendingWithValue(countries))
        })
        .await
    }
}

/// A Safe that can't cover the PIX pool top-up only surfaces as a failed payment minutes into the run.
fn ensure_run_funded(balances: &Balances) -> anyhow::Result<()> {
    let (Some(ideal), Some(funding)) = (&balances.ideal_balance, &balances.funding_status) else {
        return Err(anyhow::anyhow!("worker reports no funding figures"));
    };
    let node_address = &balances.info.node_address;
    let safe_address = &balances.info.safe_address;

    if funding.gas == FundingLevel::Empty {
        let deficit = funding.xdai_deficit.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "node {node_address} holds {}, below the worker's gas floor; fund {deficit} more",
            balances.node
        ));
    }

    // The worker only shields a top-up from the Safe once the pool drops below its floor.
    let pool = balances.pix_pool.unwrap_or_default();
    let pool_is_funded = pool >= ideal.pix_min_available;
    let safe_needed = if pool_is_funded {
        Balance::zero()
    } else {
        ideal.pix_top_up
    };
    let safe = balances.safe;
    if safe.is_zero() || safe < safe_needed {
        return Err(anyhow::anyhow!(
            "safe {safe_address} holds {safe}, a run needs {safe_needed} for the PIX pool top-up (pool holds {pool}, refills below {})",
            ideal.pix_min_available
        ));
    }

    info!(%safe, %pool, %safe_needed, node = %balances.node, "funds cover a run");
    Ok(())
}

/// The destination discovered at each address; a configured-only entry does not count.
fn discovered_exits(status: &Status, addresses: &[Address]) -> Vec<Option<Destination>> {
    addresses
        .iter()
        .map(|address| {
            status
                .destinations
                .iter()
                .map(|state| &state.destination)
                .find(|dest| {
                    let from_discovery = matches!(
                        dest.source,
                        DestinationSource::Discovered | DestinationSource::ConfiguredAndDiscovered
                    );
                    dest.address == *address && from_discovery
                })
                .cloned()
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct RankedDestination {
    pub destination: Destination,
    pub percent_of_best: u8,
}

#[derive(Debug, Clone)]
pub struct DestinationReadiness {
    /// Closest to the best exit first, then by id.
    ready: Vec<RankedDestination>,
    not_ready: Vec<Destination>,
}

impl DestinationReadiness {
    fn from_states(states: Vec<DestinationStatus>) -> Self {
        let mut ready = Vec::new();
        let mut not_ready = Vec::new();

        for state in states {
            let is_routable = state
                .route_health
                .as_ref()
                .is_some_and(|rh| matches!(rh.state, RouteHealthState::Routable));
            let percent_of_best = state.route_health.and_then(|rh| rh.percent_of_best);
            match percent_of_best {
                Some(percent_of_best) if is_routable => ready.push(RankedDestination {
                    destination: state.destination,
                    percent_of_best,
                }),
                _ => not_ready.push(state.destination),
            }
        }
        ready.sort_by(|a, b| {
            b.percent_of_best
                .cmp(&a.percent_of_best)
                .then_with(|| a.destination.id.to_string().cmp(&b.destination.id.to_string()))
        });

        Self { ready, not_ready }
    }

    pub fn ready(&self) -> &[RankedDestination] {
        &self.ready
    }

    pub fn not_ready(&self) -> &[Destination] {
        &self.not_ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnosis_vpn_ipc::{BalanceRecommendation, FundingStatus, NodeInfo};

    fn piz_palu_dev(safe: &str, pool: &str) -> Balances {
        Balances {
            node: "0.01".parse().unwrap(),
            safe: safe.parse().unwrap(),
            channels_out: Vec::new(),
            info: NodeInfo {
                node_address: Address::new([1; 20]),
                node_peer_id: "12D3KooW".into(),
                safe_address: Address::new([2; 20]),
            },
            capacity_allocations: None,
            ideal_balance: Some(BalanceRecommendation {
                wxhopr: Balance::zero(),
                xdai: "0.005".parse().unwrap(),
                channel_stakes: Balance::zero(),
                pix_pool: Balance::zero(),
                fee_to_start: Balance::zero(),
                txs_to_start: 0,
                xdai_fee_per_tx: Balance::zero(),
                topup_headroom: Balance::zero(),
                pix_top_up: "0.0001".parse().unwrap(),
                pix_min_available: "0.00002".parse().unwrap(),
            }),
            funding_status: Some(FundingStatus {
                traffic: FundingLevel::Good,
                gas: FundingLevel::Good,
                wxhopr_deficit: None,
                xdai_deficit: None,
                refill_shortfall: None,
            }),
            pix_pool: Some(pool.parse().unwrap()),
            pix_reserved: None,
        }
    }

    #[test]
    fn safe_below_the_top_up_fails_naming_safe_balance_and_need() {
        let error = ensure_run_funded(&piz_palu_dev("0.000077", "0"))
            .unwrap_err()
            .to_string();

        assert!(error.contains(&Address::new([2; 20]).to_string()), "{error}");
        assert!(error.contains("holds 0.000077 wxHOPR"), "{error}");
        assert!(error.contains("needs 0.0001 wxHOPR"), "{error}");
    }

    #[test]
    fn safe_covering_the_top_up_passes() {
        assert!(ensure_run_funded(&piz_palu_dev("0.02", "0")).is_ok());
    }

    #[test]
    fn funded_pool_needs_no_top_up_from_the_safe() {
        assert!(ensure_run_funded(&piz_palu_dev("0.000077", "0.00005")).is_ok());
    }

    #[test]
    fn pool_below_its_floor_needs_the_top_up() {
        assert!(ensure_run_funded(&piz_palu_dev("0.000077", "0.00001")).is_err());
    }

    #[test]
    fn without_a_pool_any_safe_funds_pass() {
        let mut balances = piz_palu_dev("0.000077", "0");
        balances.pix_pool = None;
        if let Some(ideal) = balances.ideal_balance.as_mut() {
            ideal.pix_top_up = Balance::zero();
            ideal.pix_min_available = Balance::zero();
        }
        assert!(ensure_run_funded(&balances).is_ok());
    }

    #[test]
    fn empty_safe_fails() {
        assert!(ensure_run_funded(&piz_palu_dev("0", "0.00005")).is_err());
    }

    #[test]
    fn node_gas_below_the_floor_fails_naming_the_node() {
        let mut balances = piz_palu_dev("0.02", "0");
        balances.node = "0.001".parse().unwrap();
        if let Some(funding) = balances.funding_status.as_mut() {
            funding.gas = FundingLevel::Empty;
            funding.xdai_deficit = Some("0.004".parse().unwrap());
        }
        let error = ensure_run_funded(&balances).unwrap_err().to_string();

        assert!(error.contains(&Address::new([1; 20]).to_string()), "{error}");
        assert!(error.contains("0.004"), "{error}");
    }
}
