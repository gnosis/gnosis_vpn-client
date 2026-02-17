use gnosis_vpn_lib::command::{
    BalanceResponse, Command, ConnectResponse, ConnectionState, DestinationState, DisconnectResponse, Response,
    RunMode, StatusResponse,
};
use gnosis_vpn_lib::connection::destination::{Address, Destination};
use gnosis_vpn_lib::connectivity_health::Health;
use gnosis_vpn_lib::hopr::hopr_lib::ToHex;
use gnosis_vpn_lib::socket::root::{Error as SocketError, process_cmd};
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

    /// Sends a raw command to the control socket and returns the daemon response.
    pub async fn send(&self, cmd: &Command) -> anyhow::Result<Response> {
        match process_cmd(self.socket_path.as_path(), cmd).await {
            Ok(resp) => Ok(resp),
            Err(SocketError::ServiceNotRunning) => {
                error!(?cmd, "service not running when sending command");
                Err(SocketError::ServiceNotRunning.into())
            }
            Err(error) => {
                error!(%error, ?cmd, "error while sending command");
                Err(error.into())
            }
        }
    }

    /// Verifies the daemon responds to ping requests.
    pub async fn ping(&self) -> anyhow::Result<()> {
        self.send(&Command::Ping).await.and_then(|result| {
            matches!(result, Response::Pong)
                .then_some(())
                .ok_or(anyhow::anyhow!("unexpected ping response {result:?}"))
        })
    }

    /// Fetches current daemon status information.
    pub async fn status(&self) -> anyhow::Result<Option<StatusResponse>> {
        match self.send(&Command::Status).await {
            Ok(Response::Status(status)) => Ok(Some(status)),
            Ok(resp) => Err(anyhow::anyhow!("unexpected status response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Retrieves the node and safe balances.
    pub async fn balance(&self) -> anyhow::Result<Option<BalanceResponse>> {
        match self.send(&Command::Balance).await {
            Ok(Response::Balance(balance)) => Ok(balance),
            Ok(resp) => Err(anyhow::anyhow!("unexpected balance response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Initiates a VPN connection to the provided destination.
    pub async fn connect(&self, destination: Address) -> anyhow::Result<ConnectResponse> {
        match self.send(&Command::Connect(destination.to_hex())).await {
            Ok(Response::Connect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected connect response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Close the VPN connection.
    pub async fn disconnect(&self) -> anyhow::Result<DisconnectResponse> {
        match self.send(&Command::Disconnect).await {
            Ok(Response::Disconnect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected disconnect response {resp:?}")),
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
                    RunMode::Init => Ok(ConditionCheck::Pending),
                    RunMode::PreparingSafe {
                        node_address: _,
                        node_xdai: _,
                        node_wxhopr: _,
                        funding_tool: _,
                    } => {
                        warn!("safe being prepared");
                        Ok(ConditionCheck::Pending)
                    }
                    _ => {
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
                Ok(Some(BalanceResponse { node, safe, .. })) => {
                    if node.is_zero() || safe.is_zero() {
                        debug!("node or safe have zero funds");
                        Ok(ConditionCheck::Pending)
                    } else {
                        info!("node and safe have funds");
                        Ok(ConditionCheck::Ready(()))
                    }
                }
                Ok(None) => Ok(ConditionCheck::Pending),
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
                    if let Some(state) = status
                        .destinations
                        .iter()
                        .find(|c| c.destination.address == destination.address)
                    {
                        let location = state
                            .destination
                            .get_meta("location")
                            .unwrap_or("<unknown>".to_string());
                        match &state.connection_state {
                            ConnectionState::Connecting(_, phase) => {
                                warn!(?phase, ?location, "connection is being established");
                                Ok(ConditionCheck::Pending)
                            }
                            ConnectionState::Connected(_) => {
                                info!(?location, "connection established successfully");
                                Ok(ConditionCheck::Ready(()))
                            }
                            _ => {
                                warn!(?location, "connection state is unknown");
                                Ok(ConditionCheck::Pending)
                            }
                        }
                    } else {
                        warn!("destination not found in status response");
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
                    DisconnectResponse::Disconnecting(address) => {
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
}

#[derive(Debug, Clone)]
pub struct DestinationReadiness {
    ready: Vec<Destination>,
    not_ready: Vec<Destination>,
}

impl DestinationReadiness {
    fn from_states(states: Vec<DestinationState>) -> Self {
        let mut ready = Vec::new();
        let mut not_ready = Vec::new();

        for state in states {
            if state.connectivity.health == Health::ReadyToConnect {
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
