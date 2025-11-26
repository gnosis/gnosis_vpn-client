use std::path::PathBuf;
use std::time::Duration;

use gnosis_vpn_lib::command::{
    BalanceResponse, Command, ConnectResponse, ConnectionState, Response, RunMode, StatusResponse,
};
use gnosis_vpn_lib::connection::destination::{Address, Destination};
use gnosis_vpn_lib::connection::destination_health::Health;
use gnosis_vpn_lib::socket;
use tracing::{debug, error, info, warn};

use crate::fixtures::lib;

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
        match socket::process_cmd(self.socket_path.as_path(), cmd).await {
            Ok(resp) => {
                debug!("got response to command {cmd:?}: {resp:?}");
                Ok(resp)
            }
            Err(socket::Error::ServiceNotRunning) => {
                error!("service not running when sending command {cmd:?}");
                Err(socket::Error::ServiceNotRunning.into())
            }
            Err(err) => {
                error!("error while sending command {cmd:?}: {err:?}");
                Err(err.into())
            }
        }
    }

    /// Verifies the daemon responds to ping requests.
    pub async fn ping(&self) -> anyhow::Result<()> {
        match self.send(&Command::Ping).await {
            Ok(Response::Pong) => Ok(()),
            Ok(resp) => Err(anyhow::anyhow!("unexpected ping response {resp:?}")),
            Err(e) => Err(e),
        }
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
        match self.send(&Command::Connect(destination)).await {
            Ok(Response::Connect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected connect response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    /// Waits until the control API responds to ping requests.
    pub async fn wait_for_service_running(&self) -> anyhow::Result<()> {
        lib::wait_for_condition(
            "service running",
            Duration::from_secs(30),
            Duration::from_secs(2),
            || async {
                match self.ping().await {
                    Ok(_) => {
                        info!("gnosis_vpn service is pingable");
                        Ok(Some(()))
                    }
                    Err(_) => Ok(None),
                }
            },
        )
        .await?;
        Ok(())
    }

    /// Ensures both on-chain accounts have funds before the test proceeds.
    pub async fn wait_for_node_funding(&self) -> anyhow::Result<()> {
        lib::wait_for_condition(
            "node funds",
            Duration::from_secs(30),
            Duration::from_secs(5),
            || async {
                match self.balance().await {
                    Ok(Some(BalanceResponse { node, safe, .. })) => {
                        if node.is_zero() || safe.is_zero() {
                            Ok(None)
                        } else {
                            Ok(Some(()))
                        }
                    }
                    Ok(None) => Ok(None),
                    Err(_) => Ok(None),
                }
            },
        )
        .await?;
        Ok(())
    }

    /// Waits until the node reports a running state.
    pub async fn wait_for_node_running(&self) -> anyhow::Result<()> {
        lib::wait_for_condition(
            "node running",
            Duration::from_secs(60 * 30),
            Duration::from_secs(10),
            || async {
                match self.status().await {
                    Ok(Some(status)) => {
                        if matches!(status.run_mode, RunMode::Running { .. }) {
                            info!("node is in Running state");
                            Ok(Some(()))
                        } else {
                            Ok(None)
                        }
                    }
                    Ok(None) => Ok(None),
                    Err(_) => Ok(None),
                }
            },
        )
        .await?;
        Ok(())
    }

    /// Returns the destinations ready for connection establishment.
    pub async fn wait_for_ready_destinations(&self) -> anyhow::Result<Vec<Destination>> {
        lib::wait_for_condition(
            "node ready to connect destinations",
            Duration::from_secs(60 * 2),
            Duration::from_secs(10),
            || async {
                match self.status().await {
                    Ok(Some(status)) => {
                        let ready_dests = status
                            .destinations
                            .iter()
                            .filter_map(|dest| {
                                dest.health.as_ref().and_then(|health| {
                                    if health.health == Health::ReadyToConnect {
                                        Some(dest.destination.clone())
                                    } else {
                                        None
                                    }
                                })
                            })
                            .collect::<Vec<Destination>>();

                        if ready_dests.is_empty() {
                            warn!("didn't find any destinations ready to connect yet");
                            Ok(None)
                        } else {
                            Ok(Some(ready_dests))
                        }
                    }
                    Ok(None) => Ok(None),
                    Err(_) => Ok(None),
                }
            },
        )
        .await
    }

    /// Ensures a specific destination reaches the Connected state.
    pub async fn wait_for_connection_established(&self, destination: &Destination) -> anyhow::Result<()> {
        lib::wait_for_condition(
            "connection established",
            Duration::from_secs(60),
            Duration::from_secs(2),
            || async {
                match self.status().await {
                    Ok(Some(status)) => {
                        if let Some(state) = status
                            .destinations
                            .iter()
                            .find(|c| c.destination.address == destination.address)
                        {
                            if matches!(state.connection_state, ConnectionState::Connected(_)) {
                                info!("connection is established");
                                return Ok(Some(()));
                            }
                            warn!(
                                "connection not established yet, current state: {:?}",
                                state.connection_state
                            );
                        }
                        Ok(None)
                    }
                    Ok(None) => Ok(None),
                    Err(_) => Ok(None),
                }
            },
        )
        .await?;
        Ok(())
    }
}
