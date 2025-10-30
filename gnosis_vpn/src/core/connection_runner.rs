use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use gnosis_vpn_lib::balance::Balances;
use gnosis_vpn_lib::connection::destination::{self, Destination};
use gnosis_vpn_lib::connection::options::{self, Options};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config};
use gnosis_vpn_lib::info::Info;
use gnosis_vpn_lib::session::{self, Protocol, Session, to_surb_balancer_config};
use gnosis_vpn_lib::wg_tooling;

use std::sync::Arc;

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct ConnectionRunner {
    destination: Destination,
    wg: wg_tooling::WireGuard,
    options: Options,
}

#[derive(Debug)]
pub enum Cmd {
    Shutdown {
        rsp: oneshot::Sender<()>,
    },
    Status,
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
    Status(HoprState),
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

impl ConnectionRunner {
    pub fn new(destination: Destination, wg: wg_tooling::WireGuard, options: Options) -> Self {
        Self {
            destination,
            wg,
            options,
        }
    }

    pub fn has_destination(&self, destination: &Destination) -> bool {
        self.destination.address == destination.address
    }

    pub fn destination(&self) -> Destination {
        self.destination.clone()
    }

    pub async fn start(
        &self,
        cmd_receiver: &mut mpsc::Receiver<Cmd>,
        evt_sender: mpsc::Sender<Evt>,
    ) -> Result<(), Error> {
        // let _ = evt_sender.send(Evt::Ready).await;
        tokio::spawn(async move {
            let _ = evt_sender.send(Evt::Ready).await;
        });
        while let Some(cmd) = cmd_receiver.recv().await {
            match cmd {
                Cmd::Shutdown { rsp } => {
                    hoprd.shutdown().await;
                    let _ = rsp.send(());
                    break;
                }
                Cmd::Status => {
                    let status = hoprd.status();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let _ = evt_sender.send(Evt::Status(status)).await;
                    });
                }
                Cmd::Info { rsp } => {
                    let info = hoprd.info();
                    let _ = rsp.send(info);
                }
                Cmd::Balances => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
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
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res = fund_channel(&hoprd, address, amount, threshold).await;
                        let _ = evt_sender.send(Evt::FundChannel { address, res }).await;
                    });
                }
            }
        }
        Ok(())
    }

    fn bridge_session_params(&self) -> session::OpenSession {
        session::OpenSession::bridge(
            self.edgli.clone(),
            self.destination.address,
            self.options.sessions.bridge.capabilities,
            self.destination.routing.clone(),
            self.options.sessions.bridge.target.clone(),
            to_surb_balancer_config(self.options.buffer_sizes.bridge, self.options.max_surb_upstream.bridge),
        )
    }

    fn ping_session_params(&self) -> session::OpenSession {
        session::OpenSession::main(
            self.edgli.clone(),
            self.destination.address,
            self.options.sessions.wg.capabilities,
            self.destination.routing.clone(),
            self.options.sessions.wg.target.clone(),
            to_surb_balancer_config(self.options.buffer_sizes.ping, self.options.max_surb_upstream.ping),
        )
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
