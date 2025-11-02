use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Address, Balance, IpProtocol, SessionClientConfig, SessionTarget, SurbBalancerConfig, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use gnosis_vpn_lib::balance::Balances;
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config, types::SessionClientMetadata};
use gnosis_vpn_lib::info::Info;

use std::net::SocketAddr;
use std::sync::Arc;

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct HoprRunner {
    hopr_params: HoprParams,
    ticket_value: Balance<WxHOPR>,
}

pub enum Evt {
    Ready(Arc<Hopr>),
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

    pub async fn start(&self, evt_sender: mpsc::Sender<Evt>) -> Result<Arc<Hopr>, Error> {
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
        Hopr::new(cfg, keys).await
    }
    /*
        let _ = evt_sender.send(Evt::Ready(hoprd.clone())).await;
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
                Cmd::OpenSession {
                    id,
                    destination,
                    target,
                    session_pool,
                    max_client_sessions,
                    cfg,
                } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res =
                            open_session(&hoprd, destination, &target, session_pool, max_client_sessions, &cfg).await;
                        let _ = evt_sender.send(Evt::OpenSession { id, res }).await;
                    });
                }
                Cmd::CloseSession {
                    id,
                    bound_session,
                    protocol,
                } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res = hoprd.close_session(bound_session, protocol).await.map_err(Error::from);
                        let _ = evt_sender.send(Evt::CloseSession { id, res }).await;
                    });
                }
                Cmd::ListSessions { protocol } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let sessions = hoprd.list_sessions(protocol).await;
                        let _ = evt_sender.send(Evt::ListSessions(sessions)).await;
                    });
                }
                Cmd::AdjustSession {
                    id,
                    balancer_cfg,
                    client,
                } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res = hoprd.adjust_session(balancer_cfg, client).await.map_err(Error::from);
                        let _ = evt_sender.send(Evt::AdjustSession { id, res }).await;
                    });
                }
            }
        }
        Ok(())
    }
            */
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

async fn open_session(
    hoprd: &Hopr,
    destination: Address,
    target: &SessionTarget,
    session_pool: Option<usize>,
    max_client_sessions: Option<usize>,
    cfg: &SessionClientConfig,
) -> Result<SessionClientMetadata, Error> {
    retry(ExponentialBackoff::default(), || async {
        let res = hoprd
            .open_session(
                destination,
                target.clone(),
                session_pool,
                max_client_sessions,
                cfg.clone(),
            )
            .await
            .map_err(Error::from)?;
        Ok(res)
    })
    .await
}
