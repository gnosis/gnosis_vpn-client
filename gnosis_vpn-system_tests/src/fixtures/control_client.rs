use std::path::PathBuf;

use gnosis_vpn_lib::command::{self, Command as GnosisCommand, ConnectResponse, Response, StatusResponse};
use gnosis_vpn_lib::connection::destination::Address;
use gnosis_vpn_lib::socket;
use tracing::{debug, error};

pub struct ControlClient {
    socket_path: PathBuf,
}

impl ControlClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn send(&self, cmd: &GnosisCommand) -> anyhow::Result<Response> {
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

    pub async fn ping(&self) -> anyhow::Result<()> {
        match self.send(&GnosisCommand::Ping).await {
            Ok(Response::Pong) => Ok(()),
            Ok(resp) => Err(anyhow::anyhow!("unexpected ping response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    pub async fn status(&self) -> anyhow::Result<Option<StatusResponse>> {
        match self.send(&GnosisCommand::Status).await {
            Ok(Response::Status(status)) => Ok(Some(status)),
            Ok(resp) => Err(anyhow::anyhow!("unexpected status response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    pub async fn balance(&self) -> anyhow::Result<Option<command::BalanceResponse>> {
        match self.send(&GnosisCommand::Balance).await {
            Ok(Response::Balance(balance)) => Ok(balance),
            Ok(resp) => Err(anyhow::anyhow!("unexpected balance response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    #[allow(dead_code)]
    pub async fn refresh_node(&self) -> anyhow::Result<()> {
        match self.send(&GnosisCommand::RefreshNode).await {
            Ok(Response::Empty) => Ok(()),
            Ok(resp) => Err(anyhow::anyhow!("unexpected refresh response {resp:?}")),
            Err(e) => Err(e),
        }
    }

    pub async fn connect(&self, destination: Address) -> anyhow::Result<ConnectResponse> {
        match self.send(&GnosisCommand::Connect(destination)).await {
            Ok(Response::Connect(state)) => Ok(state),
            Ok(resp) => Err(anyhow::anyhow!("unexpected connect response {resp:?}")),
            Err(e) => Err(e),
        }
    }
}
