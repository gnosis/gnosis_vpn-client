use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use gnosis_vpn_lib::hopr::{Hopr, HoprError, config as hopr_config};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct HoprRunner {
    hopr_params: HoprParams,
    ticket_value: Balance<WxHOPR>,
}

#[derive(Debug)]
pub enum Cmd {
    Shutdown { rsp: oneshot::Sender<()> },
    Status { rsp: oneshot::Sender<HoprState> },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    HoprConfig(#[from] hopr_config::Error),
    #[error(transparent)]
    Hopr(#[from] HoprError),
}

impl HoprRunner {
    pub fn new(hopr_params: HoprParams, ticket_value: Balance<WxHOPR>) -> Self {
        Self {
            hopr_params,
            ticket_value,
        }
    }

    pub async fn start(&self, cmd_receiver: &mut mpsc::Receiver<Cmd>) -> Result<(), Error> {
        let cfg = match self.hopr_params.config_mode.clone() {
            // use user provided configuration path
            hopr_params::ConfigFileMode::Manual(path) => hopr_config::from_path(path.as_ref())?,
            // check status of config generation
            hopr_params::ConfigFileMode::Generated => hopr_config::generate(
                self.hopr_params.network.clone(),
                self.hopr_params.rpc_provider.clone(),
                self.ticket_value,
            )?,
        };
        let keys = self.hopr_params.calc_keys()?;
        let mut hoprd = Hopr::new(cfg, keys).await?;
        while let Some(cmd) = cmd_receiver.recv().await {
            match cmd {
                Cmd::Shutdown { rsp } => {
                    hoprd.shutdown().await;
                    rsp.send(());
                    break;
                }
                Cmd::Status { rsp } => {
                    rsp.send(hoprd.status());
                }
            }
        }
        Ok(())
    }
}
