use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client;
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::hopr::{Hopr, HoprError};
use gnosis_vpn_lib::{ping, wg_tooling};

use crate::core::conn::{self, Conn};
use crate::core::runner::Results;

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

pub struct DisconnectionRunner {
    conn: Conn,
    hopr: Arc<Hopr>,
    options: Options,
}

#[derive(Debug)]
pub enum Evt {
    DisconnectWg,
    OpenBridge,
    UnregisterWg,
    CloseBridge,
}

impl DisconnectionRunner {
    pub fn new(conn: Conn, hopr: Arc<Hopr>, options: Options) -> Self {
        Self { conn, hopr, options }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender
            .send(Results::DisconnectionResult { id: self.conn.id, res })
            .await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. disconnect wg tunnel if any
        if let Some(wg) = self.conn.wg() {
            let _ = results_sender
                .send(Results::DisconnectionEvent {
                    id: self.conn.id,
                    evt: Evt::DisconnectWg,
                })
                .await;
            let _ = wg
                .close_session()
                .await
                .map_err(|err| tracing::warn!("disconnecting WireGuard failed: {}", err));
        }

        // run unregister flow if we have a public key
        if let Some(public_key) = self.conn.wg_public_key() {
            let _ = results_sender
                .send(Results::DisconnectionEvent {
                    id: self.conn.id,
                    evt: Evt::OpenBridge,
                })
                .await;
            // 1. open bridge session
            let bridge_session = open_bridge_session(&self.hopr, &self.conn, &self.options).await?;

            // 2. unregister wg public key
            let _ = results_sender
                .send(Results::DisconnectionEvent {
                    id: self.conn.id,
                    evt: Evt::UnregisterWg,
                })
                .await;
            match unregister(&self.options, &bridge_session, public_key).await {
                Ok(_) => (),
                Err(gvpn_client::Error::RegistrationNotFound) => {
                    tracing::warn!("trying to unregister already removed registration");
                }
                Err(error) => {
                    tracing::error!(%error, "unregistering from gvpn server failed");
                }
            }
            // 3. close bridge session
            let _ = results_sender
                .send(Results::DisconnectionEvent {
                    id: self.conn.id,
                    evt: Evt::CloseBridge,
                })
                .await;
            close_bridge_session(&self.hopr, &bridge_session).await?;
        }

        Ok(())
    }
}

async fn open_bridge_session(hopr: &Hopr, conn: &Conn, options: &Options) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: conn.destination.routing.clone(),
        return_path_options: conn.destination.routing.clone(),
        surb_management: Some(conn::to_surb_balancer_config(
            options.buffer_sizes.bridge,
            options.max_surb_upstream.bridge,
        )),
        ..Default::default()
    };
    hopr.open_session(
        conn.destination.address,
        options.sessions.bridge.target.clone(),
        Some(1),
        Some(1),
        cfg.clone(),
    )
    .await
}

async fn unregister(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
    public_key: String,
) -> Result<(), gvpn_client::Error> {
    let input = gvpn_client::Input::new(
        public_key,
        session_client_metadata.bound_host.port(),
        options.timeouts.http,
    );
    let client = reqwest::Client::new();
    gvpn_client::unregister(&client, &input).await
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

impl Display for DisconnectionRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DisconnectionRunner {{ {} }}", self.conn)
    }
}

impl Display for Evt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Evt::DisconnectWg => write!(f, "DisconnectWg"),
            Evt::OpenBridge => write!(f, "OpenBridge"),
            Evt::UnregisterWg => write!(f, "UnregisterWg"),
            Evt::CloseBridge => write!(f, "CloseBridge"),
        }
    }
}
