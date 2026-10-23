use gnosis_vpn_ipc::{
    self as ipc, Address, Balances, CallError, ConnectResponse, Connection, Destination, DestinationId,
    DestinationSource, DestinationStatus, DisconnectResponse, Request, Response, RouteHealthState, RunMode,
    StartClientResponse, Status, StopClientResponse,
};
use rand::seq::SliceRandom;
use std::{path::PathBuf, time::Duration};
use tracing::{debug, error, info, warn};

use crate::fixtures::lib::{self, ConditionCheck};

/// Thin wrapper around the gnosis_vpn control socket used during system tests.
pub struct ControlClient {
    socket_path: PathBuf,
}

impl ControlClient {
    /// Creates a new client bound to a Unix domain socket path.
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
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

    /// Start the client
    pub async fn start(&self) -> anyhow::Result<StartClientResponse> {
        match self
            .send(Request::StartClient {
                timeout: Duration::from_mins(10),
            })
            .await
        {
            Ok(Response::StartClient(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected start response {resp:?}")),
            Err(e) => Err(e),
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

    /// Ensures both on-chain accounts have funds before the test proceeds.
    pub async fn wait_for_node_funding(&self, timeout: Duration) -> anyhow::Result<()> {
        lib::wait_for_condition("node funds", timeout, Duration::from_secs(5), || async {
            match self.balance().await {
                Ok(Balances { node, safe, .. }) => {
                    if node.is_zero() || safe.is_zero() {
                        debug!("node or safe have zero funds");
                        Ok(ConditionCheck::Pending)
                    } else {
                        info!("node and safe have funds");
                        Ok(ConditionCheck::Ready(()))
                    }
                }
                Err(_) => Ok(ConditionCheck::Pending),
            }
        })
        .await?;
        Ok(())
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

    /// Aggregates ready and not-ready destinations.
    pub async fn wait_for_ready_destinations(&self, timeout: Duration) -> anyhow::Result<DestinationReadiness> {
        lib::wait_for_condition(
            "node ready to reach all destinations",
            timeout,
            Duration::from_secs(10),
            || async {
                match self.status().await {
                    Ok(Some(status)) => {
                        let mut readiness = DestinationReadiness::from_states(status.destinations);

                        if readiness.ready().is_empty() {
                            warn!("no ready destinations yet");
                            return Ok(ConditionCheck::PendingWithValue(readiness));
                        }

                        if readiness.not_ready().is_empty() {
                            info!("all destinations are ready");
                            readiness.shuffle_ready();
                            return Ok(ConditionCheck::Ready(readiness));
                        }

                        warn!(
                            ready = readiness.ready().len(),
                            not_ready = readiness.not_ready().len(),
                            "waiting for all destinations to be ready"
                        );
                        Ok(ConditionCheck::PendingWithValue(readiness))
                    }
                    Ok(None) => Ok(ConditionCheck::Pending),
                    Err(_) => Ok(ConditionCheck::Pending),
                }
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
pub struct DestinationReadiness {
    ready: Vec<Destination>,
    not_ready: Vec<Destination>,
}

impl DestinationReadiness {
    fn from_states(states: Vec<DestinationStatus>) -> Self {
        let mut ready = Vec::new();
        let mut not_ready = Vec::new();

        for state in states {
            let is_ready = state
                .route_health
                .as_ref()
                .is_some_and(|rh| matches!(rh.state, RouteHealthState::Routable));
            if is_ready {
                ready.push(state.destination);
            } else {
                not_ready.push(state.destination);
            }
        }

        Self { ready, not_ready }
    }

    pub fn ready(&self) -> &[Destination] {
        &self.ready
    }

    pub fn not_ready(&self) -> &[Destination] {
        &self.not_ready
    }

    pub fn shuffle_ready(&mut self) {
        self.ready.shuffle(&mut rand::rng());
    }
}
