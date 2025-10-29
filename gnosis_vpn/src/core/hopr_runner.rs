use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use gnosis_vpn_lib::balance::{self, Balances};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config};
use gnosis_vpn_lib::info::{self, Info};

use std::sync::Arc;

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
    Info {
        rsp: oneshot::Sender<Info>,
    },
    Balances,
    FundChannel {
        address: Address,
        amount: Balance<WxHOPR>,
        threshold: Balance<WxHOPR>,
    },
}

#[derive(Debug)]
pub enum Evt {
    Ready,
    FundChannel { address: Address, res: Result<(), Error> },
    Balances(Result<Balances, Error>),
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
    Channel(#[from] hopr_api::ChannelError),
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
        let edgli = Hopr::new(cfg, keys).await?;
        let hoprd = Arc::new(edgli);
        let _ = evt_sender.send(Evt::Ready).await;
        while let Some(cmd) = cmd_receiver.recv().await {
            match cmd {
                Cmd::Shutdown { rsp } => {
                    hoprd.shutdown().await;
                    let _ = rsp.send(());
                    break;
                }
                Cmd::Status { rsp } => {
                    let _ = rsp.send(hoprd.status());
                }
                Cmd::Info { rsp } => {
                    let info = hoprd.info();
                    let _ = rsp.send(info);
                }
                Cmd::Balances => {
                    let hoprd = hoprd.clone();
                    tokio::spawn(async {
                        let res = hoprd.balances().await.map_err(Error::from);
                        let _ = evt_sender.send(Evt::Balances(res)).await;
                    });
                }
                Cmd::FundChannel {
                    address,
                    amount,
                    threshold,
                } => {
                    let hoprd = hoprd.clone();
                    tokio::spawn(async {
                        let res = fund_channel(&hoprd, address, amount, threshold).await;
                        let _ = evt_sender.send(Evt::FundChannel { address, res }).await;
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
