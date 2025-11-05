use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client::{self, Registration};
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::hopr::{Hopr, HoprError};
use gnosis_vpn_lib::{ping, wg_tooling};

use crate::core::conn::Conn;
use crate::core::runner::{self, Results};

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

pub struct ConnectionRunner {
    conn: Conn,
    hopr: Arc<Hopr>,
    options: Options,
    wg_config: wg_tooling::Config,
}

#[derive(Debug)]
pub enum Evt {
    GenerateWg,
    OpenBridge,
    RegisterWg(String),
    CloseBridge,
    OpenPing,
    WgTunnel(wg_tooling::WireGuard),
    Ping,
    AdjustToMain,
}

impl ConnectionRunner {
    pub fn new(conn: Conn, options: Options, wg_config: wg_tooling::Config, hopr: Arc<Hopr>) -> Self {
        Self {
            conn,
            hopr,
            options,
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
            .send(Results::ConnectionEvent { evt: Evt::GenerateWg })
            .await;
        let wg = wg_tooling::WireGuard::from_config(self.wg_config.clone()).await?;

        // 1. open bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent { evt: Evt::OpenBridge })
            .await;
        let bridge_session = open_bridge_session(&self.hopr, &self.conn, &self.options).await?;

        // 2. register wg public key
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: Evt::RegisterWg(wg.key_pair.public_key.clone()),
            })
            .await;
        let registration = register(&self.options, &bridge_session, &wg).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent { evt: Evt::CloseBridge })
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        // 4. open ping session
        let _ = results_sender
            .send(Results::ConnectionEvent { evt: Evt::OpenPing })
            .await;
        let ping_session = open_ping_session(&self.hopr, &self.conn, &self.options).await?;

        // 5. setup wg tunnel
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: Evt::WgTunnel(wg.clone()),
            })
            .await;
        wg_tunnel(&registration, &ping_session, &wg).await?;

        // 6. check ping
        let _ = results_sender.send(Results::ConnectionEvent { evt: Evt::Ping }).await;
        ping(&self.options).await?;

        // 7. adjust to main session
        let _ = results_sender
            .send(Results::ConnectionEvent { evt: Evt::AdjustToMain })
            .await;
        adjust_to_main_session(&self.hopr, &self.options, &ping_session).await?;

        Ok(())
    }
}

impl Display for ConnectionRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionRunner {{ {} }}", self.conn)
    }
}

impl Display for Evt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Evt::GenerateWg => write!(f, "GenerateWg"),
            Evt::OpenBridge => write!(f, "OpenBridge"),
            Evt::RegisterWg(_) => write!(f, "RegisterWg"),
            Evt::CloseBridge => write!(f, "CloseBridge"),
            Evt::OpenPing => write!(f, "OpenPing"),
            Evt::WgTunnel(_) => write!(f, "WgTunnel"),
            Evt::Ping => write!(f, "Ping"),
            Evt::AdjustToMain => write!(f, "AdjustToMain"),
        }
    }
}

async fn open_bridge_session(hopr: &Hopr, conn: &Conn, options: &Options) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: conn.destination.routing.clone(),
        return_path_options: conn.destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.bridge,
            options.max_surb_upstream.bridge,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        let res = hopr
            .open_session(
                conn.destination.address,
                options.sessions.bridge.target.clone(),
                Some(1),
                Some(1),
                cfg.clone(),
            )
            .await?;
        Ok(res)
    })
    .await
}

async fn register(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
    wg: &wg_tooling::WireGuard,
) -> Result<Registration, gvpn_client::Error> {
    let input = gvpn_client::Input::new(
        wg.key_pair.public_key.clone(),
        session_client_metadata.bound_host.port(),
        options.timeouts.http,
    );
    let client = reqwest::Client::new();
    retry(ExponentialBackoff::default(), || async {
        let res = gvpn_client::register(&client, &input).await?;
        Ok(res)
    })
    .await
}

async fn close_bridge_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
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

async fn open_ping_session(hopr: &Hopr, conn: &Conn, options: &Options) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path_options: conn.destination.routing.clone(),
        return_path_options: conn.destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.ping,
            options.max_surb_upstream.ping,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        let res = hopr
            .open_session(
                conn.destination.address,
                options.sessions.wg.target.clone(),
                None,
                None,
                cfg.clone(),
            )
            .await?;
        Ok(res)
    })
    .await
}

async fn wg_tunnel(
    registration: &Registration,
    session_client_metadata: &SessionClientMetadata,
    wg: &wg_tooling::WireGuard,
) -> Result<(), wg_tooling::Error> {
    // run wg-quick down once to ensure no dangling state
    _ = wg.close_session().await;

    let interface_info = wg_tooling::InterfaceInfo {
        address: registration.address(),
        mtu: session_client_metadata.hopr_mtu,
    };

    let peer_info = wg_tooling::PeerInfo {
        public_key: registration.server_public_key(),
        endpoint: format!("127.0.0.1:{}", session_client_metadata.bound_host.port()),
    };

    wg.connect_session(&interface_info, &peer_info).await
}

async fn ping(options: &Options) -> Result<(), ping::Error> {
    ping::ping(&options.ping_options)
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
    let surb_management = runner::to_surb_balancer_config(options.buffer_sizes.main, options.max_surb_upstream.main);
    hopr.adjust_session(surb_management, active_client).await
}
