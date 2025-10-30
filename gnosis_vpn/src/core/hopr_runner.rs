use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Address, Balance, IpProtocol, SessionClientConfig, SessionTarget, SurbBalancerConfig, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

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
    OpenSession {
        destination: Address,
        target: SessionTarget,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        cfg: SessionClientConfig,
    },
    CloseSession {
        bound_session: SocketAddr,
        protocol: IpProtocol,
    },
    ListSessions {
        protocol: IpProtocol,
    },
    AdjustSession {
        balancer_cfg: SurbBalancerConfig,
        client: String,
    },
}

#[derive(Debug)]
pub enum Evt {
    Ready,
    FundChannel { address: Address, res: Result<(), Error> },
    Balances(Result<Balances, Error>),
    Status(HoprState),
    OpenSession(Result<SessionClientMetadata, Error>),
    CloseSession(Result<(), Error>),
    ListSessions(Vec<SessionClientMetadata>),
    AdjustSession(Result<(), Error>),
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
                        let _ = evt_sender.send(Evt::OpenSession(res)).await;
                    });
                }
                Cmd::CloseSession {
                    bound_session,
                    protocol,
                } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res = hoprd.close_session(bound_session, protocol).await.map_err(Error::from);
                        let _ = evt_sender.send(Evt::CloseSession(res)).await;
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
                Cmd::AdjustSession { balancer_cfg, client } => {
                    let hoprd = hoprd.clone();
                    let evt_sender = evt_sender.clone();
                    tokio::spawn(async move {
                        let res = hoprd.adjust_session(balancer_cfg, client).await.map_err(Error::from);
                        let _ = evt_sender.send(Evt::AdjustSession(res)).await;
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
