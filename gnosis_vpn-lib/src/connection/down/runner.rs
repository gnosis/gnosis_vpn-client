//! The runner module for `core::connection::down` struct.
//! It handles all state transitions and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use crate::connection;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::gvpn_client;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::ping;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Hopr(#[from] HoprError),
    #[error(transparent)]
    GvpnClient(#[from] gvpn_client::Error),
    #[error(transparent)]
    Ping(#[from] ping::Error),
}

pub struct Runner {
    down: connection::down::Down,
    hopr: Arc<Hopr>,
    options: Options,
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    DisconnectWg,
    OpenBridge,
    UnregisterWg,
    CloseBridge,
}

impl Runner {
    pub fn new(down: connection::down::Down, hopr: Arc<Hopr>, options: Options) -> Self {
        Self { down, hopr, options }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender
            .send(Results::DisconnectionResult {
                wg_public_key: self.down.wg_public_key.clone(),
                res,
            })
            .await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. disconnect wg tunnel done from root
        // 1. open bridge session
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::OpenBridge,
            })
            .await;
        let bridge_session = open_bridge_session(&self.hopr, &self.down, &self.options).await?;

        // 2. unregister wg public key
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::UnregisterWg,
            })
            .await;
        match unregister(&self.options, &bridge_session, self.down.wg_public_key.clone()).await {
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
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::CloseBridge,
            })
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        Ok(())
    }
}

async fn open_bridge_session(
    hopr: &Hopr,
    down: &connection::down::Down,
    options: &Options,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: down.destination.routing.clone(),
        return_path_options: down.destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.bridge,
            options.max_surb_upstream.bridge,
        )),
        ..Default::default()
    };
    hopr.open_session(
        down.destination.address,
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

impl Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DisconnectionRunner for {}", self.down)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::DisconnectWg => write!(f, "DisconnectWg"),
            Event::OpenBridge => write!(f, "OpenBridge"),
            Event::UnregisterWg => write!(f, "UnregisterWg"),
            Event::CloseBridge => write!(f, "CloseBridge"),
        }
    }
}
