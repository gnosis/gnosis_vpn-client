use thiserror::Error;
use edgli::hopr_lib::state::HoprState;
use tokio::sync::mpsc;

use gnosis_vpn_lib::chain::contracts::NetworkSpecifications;
use gnosis_vpn_lib::ticket_stats::{self, TicketStats};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct HoprRunner {
    hopr_params: HoprParams,
}

#[derive(Debug)]
pub struct Cmd {
    Shutdown{ rsp: oneshot::Sender<()> },
    Status { rsp: oneshot::Sender<HoprState> },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
}

impl HoprRunner {
    pub fn new(hopr: Hopr) -> Self {
        Self { hopr }
    }

    pub async fn start(&self, cmd_receiver: &mut mpsc::Receiver<Cmd>) -> Result<(), Error> {
        let cfg = match self.hopr_params.config_mode.clone() {
            // use user provided configuration path
            hopr_params::ConfigFileMode::Manual(path) => hopr_config::from_path(path.as_ref())?,
            // check status of config generation
            hopr_params::ConfigFileMode::Generated => hopr_config::generate(
                self.hopr_params.network.clone(),
                self.hopr_params.rpc_provider.clone(),
                ticket_value,
            )?,
        };
        let keys = self.hopr_params.calc_keys()?;
        let hoprd = Hopr::new(cfg, keys).await?;
        while Some(cmd) = cmd_receiver.recv().await {
            match cmd {
                Cmd::Shutdown { rsp } => {
                    hoprd.shutdown().await?;
                    let _ = rsp.send(())?;
                    break;
                }
                Cmd::Status { rsp } => {
                    let state = hoprd.get_state().await?;
                    let _ = rsp.send(state)?;
                }
            }

        }
    }

}
