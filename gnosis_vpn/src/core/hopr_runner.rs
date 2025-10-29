use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::api::ChannelError;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use serde_json::json;
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
    Shutdown {
        rsp: oneshot::Sender<()>,
    },
    Status {
        rsp: oneshot::Sender<HoprState>,
    },
    FundChannel {
        address: Address,
        amount: Balance<WxHOPR>,
        threshold: Balance<WxHOPR>,
    },
}

#[derive(Debug)]
pub enum Evt {
    Ready,
    ChannelFunded(Address),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    HoprConfig(#[from] hopr_config::Error),
    #[error(transparent)]
    Hopr(#[from] HoprError),
    #[error(transparent)]
    Channel(#[from] ChannelError),
}

impl HoprRunner {
    pub fn new(hopr_params: HoprParams, ticket_value: Balance<WxHOPR>) -> Self {
        Self {
            hopr_params,
            ticket_value,
        }
    }

    pub async fn start(
        &self,
        cmd_receiver: &mut mpsc::Receiver<Cmd>,
        evt_sender: mpsc::Sender<Evt>,
    ) -> Result<(), Error> {
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
        let _ = evt_sender.send(Evt::Ready).await;
        while let Some(cmd) = cmd_receiver.recv().await {
            match cmd {
                Cmd::Shutdown { rsp } => {
                    hoprd.shutdown().await;
                    let _ = rsp.send(()).map_err(|err| {
                        tracing::warn!(?err, "failed to send shutdown response");
                    });
                    break;
                }
                Cmd::Status { rsp } => {
                    let _ = rsp.send(hoprd.status()).map_err(|err| {
                        tracing::warn!(?err, "failed responding to status request");
                    });
                }
            }
        }
        Ok(())
    }
}

async fn fund_channel(
    hoprd: &Hopr,
    address: Address,
    amount: Balance<WxHOPR>,
    threshold: Balance<WxHOPR>,
) -> Result<(), Error> {
    retry(ExponentialBackoff::default(), || async {
        hoprd
            .ensure_channel_open_and_funded(address, amount, threshold)
            .await
            .map_err(Error::from)?;
        Ok(())
    })
    .await
}
