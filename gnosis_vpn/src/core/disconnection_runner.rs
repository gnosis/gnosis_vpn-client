use backoff::ExponentialBackoff;
use backoff::future::retry;
use bytesize::ByteSize;
use edgli::hopr_lib::SessionClientConfig;
use edgli::hopr_lib::SurbBalancerConfig;
use human_bandwidth::re::bandwidth::Bandwidth;
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client;
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::hopr::{Hopr, HoprError};
use gnosis_vpn_lib::{ping, wg_tooling};

use crate::core::conn::Conn;
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
    hoprd: Arc<Hopr>,
    options: Options,
    wg: Option<wg_tooling::WireGuard>,
    wg_public_key: String,
}

#[derive(Debug)]
pub enum Evt {
    OpenBridge,
    UnregisterWg,
    CloseBridge,
}

impl DisconnectionRunner {
    pub fn new(
        conn: Conn,
        hoprd: Arc<Hopr>,
        options: Options,
        wg_public_key: String,
        wg: Option<wg_tooling::WireGuard>,
    ) -> Self {
        Self {
            conn,
            hoprd,
            options,
            wg_public_key,
            wg,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender
            .send(Results::DisconnectionResult { id: self.conn.id, res })
            .await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. disconnect wireguard
        if let Some(wg) = self.wg.clone() {
            let _ = wg
                .close_session()
                .await
                .map_err(|err| tracing::warn!("disconnecting WireGuard failed: {}", err));
        }

        // 1. open bridge session
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                id: self.conn.id,
                evt: Evt::OpenBridge,
            })
            .await;
        let bridge_session = self.open_bridge_session().await?;

        // 2. unregister wg public key
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                id: self.conn.id,
                evt: Evt::UnregisterWg,
            })
            .await;
        match self.unregister(&bridge_session).await {
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
        self.close_bridge_session(&bridge_session).await?;

        Ok(())
    }

    async fn open_bridge_session(&self) -> Result<SessionClientMetadata, HoprError> {
        let cfg = SessionClientConfig {
            capabilities: self.options.sessions.bridge.capabilities,
            forward_path_options: self.conn.destination.routing.clone(),
            return_path_options: self.conn.destination.routing.clone(),
            surb_management: Some(to_surb_balancer_config(
                self.options.buffer_sizes.bridge,
                self.options.max_surb_upstream.bridge,
            )),
            ..Default::default()
        };
        retry(ExponentialBackoff::default(), || async {
            let res = self
                .hoprd
                .open_session(
                    self.conn.destination.address,
                    self.options.sessions.bridge.target.clone(),
                    Some(1),
                    Some(1),
                    cfg.clone(),
                )
                .await?;
            Ok(res)
        })
        .await
    }

    async fn unregister(&self, session_client_metadata: &SessionClientMetadata) -> Result<(), gvpn_client::Error> {
        let input = gvpn_client::Input::new(
            self.wg_public_key.clone(),
            session_client_metadata.bound_host.port(),
            self.options.timeouts.http,
        );
        let client = reqwest::Client::new();
        let res = gvpn_client::unregister(&client, &input).await?;
        Ok(res)
    }

    async fn close_bridge_session(&self, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
        let res = self
            .hoprd
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

    async fn open_ping_session(&self) -> Result<SessionClientMetadata, HoprError> {
        let cfg = SessionClientConfig {
            capabilities: self.options.sessions.wg.capabilities,
            forward_path_options: self.conn.destination.routing.clone(),
            return_path_options: self.conn.destination.routing.clone(),
            surb_management: Some(to_surb_balancer_config(
                self.options.buffer_sizes.ping,
                self.options.max_surb_upstream.ping,
            )),
            ..Default::default()
        };
        retry(ExponentialBackoff::default(), || async {
            let res = self
                .hoprd
                .open_session(
                    self.conn.destination.address,
                    self.options.sessions.wg.target.clone(),
                    None,
                    None,
                    cfg.clone(),
                )
                .await?;
            Ok(res)
        })
        .await
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
            Evt::OpenBridge => write!(f, "Evt::OpenBridge"),
            Evt::UnregisterWg => write!(f, "Evt::UnregisterWg"),
            Evt::CloseBridge => write!(f, "Evt::CloseBridge"),
        }
    }
}

fn to_surb_balancer_config(response_buffer: ByteSize, max_surb_upstream: Bandwidth) -> SurbBalancerConfig {
    // Buffer worth at least 2 reply packets
    if response_buffer.as_u64() >= 2 * edgli::hopr_lib::SESSION_MTU as u64 {
        SurbBalancerConfig {
            target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::SESSION_MTU as u64,
            max_surbs_per_sec: (max_surb_upstream.as_bps() as usize / (8 * edgli::hopr_lib::SURB_SIZE)) as u64,
            ..Default::default()
        }
    } else {
        // Use defaults otherwise
        Default::default()
    }
}
