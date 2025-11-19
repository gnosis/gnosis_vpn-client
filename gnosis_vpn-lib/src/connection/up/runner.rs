//! The runner module for `core::connection::up` struct.
//! It handles all state transitions and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use crate::connection;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::peer::Peer;
use crate::ping;
use crate::wg_tooling::{self, InterfaceInfo};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Hopr(#[from] HoprError),
    #[error(transparent)]
    GvpnClient(#[from] gvpn_client::Error),
    #[error(transparent)]
    WgTooling(#[from] wg_tooling::Error),
    #[error(transparent)]
    Ping(#[from] ping::Error),
}

pub struct Runner {
    up: connection::up::Up,
    hopr: Arc<Hopr>,
    options: Options,
    peer: Peer,
    wg_config: wg_tooling::Config,
}

#[derive(Debug)]
pub enum Event {
    Progress(Progress),
    Setback(Setback),
}

#[derive(Debug)]
pub enum Progress {
    GenerateWg,
    OpenBridge,
    RegisterWg(String),
    CloseBridge,
    OpenPing,
    WgTunnel(wg_tooling::WireGuard),
    Ping,
    AdjustToMain,
}

#[derive(Debug)]
pub enum Setback {
    OpenBridge(String),
    RegisterWg(String),
    OpenPing(String),
}

impl Runner {
    pub fn new(
        up: connection::up::Up,
        options: Options,
        wg_config: wg_tooling::Config,
        peer: Peer,
        hopr: Arc<Hopr>,
    ) -> Self {
        Self {
            up,
            hopr,
            options,
            peer,
            wg_config,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResult { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. generate wg keys
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::GenerateWg),
            })
            .await;
        let wg = wg_tooling::WireGuard::from_config(self.wg_config.clone()).await?;

        // 1. open bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::OpenBridge),
            })
            .await;
        let bridge_session = open_bridge_session(&self.hopr, &self.up, &self.options, &results_sender).await?;

        // 2. register wg public key
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::RegisterWg(wg.key_pair.public_key.clone())),
            })
            .await;
        let registration = register(&self.options, &bridge_session, &wg, &results_sender).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::CloseBridge),
            })
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        // 4. open ping session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::OpenPing),
            })
            .await;
        let ping_session = open_ping_session(&self.hopr, &self.up, &self.options, &results_sender).await?;

        // 5. setup wg tunnel
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::WgTunnel(wg.clone())),
            })
            .await;
        wg_tunnel(&registration, &ping_session, &self.peer, &wg).await?;

        // 6. check ping
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::Ping),
            })
            .await;
        ping(&self.options).await?;

        // 7. adjust to main session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::AdjustToMain),
            })
            .await;
        adjust_to_main_session(&self.hopr, &self.options, &ping_session).await?;

        Ok(())
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionRunner {{ {} }}", self.up)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Progress(p) => write!(f, "Progress: {p}"),
            Event::Setback(s) => write!(f, "Setback: {s}"),
        }
    }
}

impl Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Progress::GenerateWg => write!(f, "Generating WireGuard keypairs"),
            Progress::OpenBridge => write!(f, "Opening bridge connection"),
            Progress::RegisterWg(pk) => write!(f, "Registering WireGuard public key {}", pk),
            Progress::CloseBridge => write!(f, "Closing bridge connection"),
            Progress::OpenPing => write!(f, "Opening main connection"),
            Progress::WgTunnel(_) => write!(f, "Establishing WireGuard tunnel"),
            Progress::Ping => write!(f, "Verifying connectivity via ping"),
            Progress::AdjustToMain => write!(f, "Adjusting to main session"),
        }
    }
}

impl Display for Setback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Setback::OpenBridge(reason) => write!(f, "Failed to open bridge session: {}", reason),
            Setback::RegisterWg(reason) => write!(f, "Failed to register WireGuard public key: {}", reason),
            Setback::OpenPing(reason) => write!(f, "Failed to open main session: {}", reason),
        }
    }
}

#[tracing::instrument(
    skip(hopr, options, up, results_sender),
    fields(
        address = %up.destination.address,
        routing = ?up.destination.routing,
        phase = ?up.phase,
        wg_public_key = ?up.wg_public_key
    ),
    level = "debug",
    ret
)]
async fn open_bridge_session(
    hopr: &Hopr,
    up: &connection::up::Up,
    options: &Options,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: up.destination.routing.clone(),
        return_path_options: up.destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.bridge,
            options.max_surb_upstream.bridge,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(%up,"attempting to open bridge session");
        let res = hopr
            .open_session(
                up.destination.address,
                options.sessions.bridge.target.clone(),
                Some(1),
                Some(1),
                cfg.clone(),
            )
            .await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::OpenBridge(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

async fn register(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
    wg: &wg_tooling::WireGuard,
    results_sender: &mpsc::Sender<Results>,
) -> Result<Registration, gvpn_client::Error> {
    let input = gvpn_client::Input::new(
        wg.key_pair.public_key.clone(),
        session_client_metadata.bound_host.port(),
        options.timeouts.http,
    );
    let client = reqwest::Client::new();
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(?input, "attempting to register gvpn client public key");
        let res = gvpn_client::register(&client, &input).await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::RegisterWg(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

async fn close_bridge_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
    tracing::debug!(
        bound_host = ?session_client_metadata.bound_host,
        "closing bridge session"
    );
    let res = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(HoprError::SessionNotFound) => {
            tracing::warn!("attempted to close bridge session but it was not found, possibly already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn open_ping_session(
    hopr: &Hopr,
    up: &connection::up::Up,
    options: &Options,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path_options: up.destination.routing.clone(),
        return_path_options: up.destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.ping,
            options.max_surb_upstream.ping,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(%up, "attempting to open ping session");
        let res = hopr
            .open_session(
                up.destination.address,
                options.sessions.wg.target.clone(),
                None,
                None,
                cfg.clone(),
            )
            .await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::OpenPing(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

async fn wg_tunnel(
    registration: &Registration,
    session_client_metadata: &SessionClientMetadata,
    peer: &Peer,
    wg: &wg_tooling::WireGuard,
) -> Result<(), wg_tooling::Error> {
    // run wg-quick down once to ensure no dangling state
    _ = wg_tooling::down().await;

    let interface_info = InterfaceInfo::from_system(&peer.ipv4).await?;

    let peer_info = wg_tooling::PeerInfo {
        public_key: registration.server_public_key(),
        port: session_client_metadata.bound_host.port(),
        relayer_ip: peer.ipv4,
    };

    tracing::debug!(%registration, "establishing wg tunnel");
    wg.up(registration.client_ip(), &interface_info, &peer_info).await
}

async fn ping(options: &Options) -> Result<(), ping::Error> {
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(?options, "attempting to ping through wg tunnel");
        ping::ping(&options.ping_options)?;
        Ok(())
    })
    .await
}

async fn adjust_to_main_session(
    hopr: &Hopr,
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
) -> Result<(), HoprError> {
    let active_client = match session_client_metadata.active_clients.as_slice() {
        [] => return Err(HoprError::SessionNotFound),
        [client] => client.clone(),
        _ => return Err(HoprError::SessionAmbiguousClient),
    };
    tracing::debug!(bound_host = ?session_client_metadata.bound_host, "adjusting to main session");
    let surb_management = runner::to_surb_balancer_config(options.buffer_sizes.main, options.max_surb_upstream.main);
    hopr.adjust_session(surb_management, active_client).await
}

fn setback(setback: Setback) -> Event {
    Event::Setback(setback)
}

fn progress(progress: Progress) -> Event {
    Event::Progress(progress)
}
