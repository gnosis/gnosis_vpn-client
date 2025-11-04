use backoff::ExponentialBackoff;
use backoff::future::retry;
use bytesize::ByteSize;
use edgli::hopr_lib::SessionClientConfig;
use edgli::hopr_lib::SurbBalancerConfig;
use human_bandwidth::re::bandwidth::Bandwidth;
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

use std::fmt::{self, Display};
use std::sync::Arc;

use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client::{self, Registration};
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

pub struct ConnectionRunner {
    hoprd: Arc<Hopr>,
    options: Options,
    wg_config: wg_tooling::Config,
}

#[derive(Debug)]
pub enum Evt {
    OpenBridge,
    Register(String),
    CloseBridge,
    OpenPing,
    WgTunnel,
    Ping,
    AdjustToMain,
}

impl ConnectionRunner {
    pub fn new(options: Options, wg_config: wg_tooling::Config, hoprd: Arc<Hopr>) -> Self {
        Self {
            hoprd,
            options,
            wg_config,
        }
    }

    pub async fn connect(&self, id: Uuid, results_sender: mpsc::Sender<Results>) {
        let res = self.run_connect(id, results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResult { id, res }).await;
    }

    pub async fn disconnect(&self, conn: Conn, results_sender: mpsc::Sender<Results>) {
        let res = self.run_disconnect(conn, results_sender.clone()).await;
        let _ = results_sender
            .send(Results::DisconnectionResult { id: conn.id, res })
            .await;
    }

    async fn run_connect(&self, id: Uuid, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. generate wg keys
        let wg = wg_tooling::WireGuard::from_config(self.wg_config.clone()).await?;

        // 1. open bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                id,
                evt: Evt::OpenBridge,
            })
            .await;
        let bridge_session = self.open_bridge_session().await?;

        // 2. register wg public key
        let _ = results_sender
            .send(Results::ConnectionEvent {
                id,
                evt: Evt::Register(wg.key_pair.public_key.clone()),
            })
            .await;
        let registration = self.register(&bridge_session, &wg).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                id,
                evt: Evt::CloseBridge,
            })
            .await;
        self.close_bridge_session(&bridge_session).await?;

        // 4. open ping session
        let _ = results_sender
            .send(Results::ConnectionEvent { id, evt: Evt::OpenPing })
            .await;
        let ping_session = self.open_ping_session().await?;

        // 5. setup wg tunnel
        let _ = results_sender
            .send(Results::ConnectionEvent { id, evt: Evt::WgTunnel })
            .await;
        self.wg_tunnel(&registration, &ping_session, &wg).await?;

        // 6. check ping
        let _ = results_sender
            .send(Results::ConnectionEvent { id, evt: Evt::Ping })
            .await;
        self.ping().await?;

        // 7. adjust to main session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                id,
                evt: Evt::AdjustToMain,
            })
            .await;
        self.adjust_to_main_session(&ping_session).await?;

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

    async fn register(
        &self,
        session_client_metadata: &SessionClientMetadata,
        wg: &wg_tooling::WireGuard,
    ) -> Result<Registration, gvpn_client::Error> {
        let input = gvpn_client::Input::new(
            wg.key_pair.public_key.clone(),
            session_client_metadata.bound_host.port(),
            self.options.timeouts.http,
        );
        let client = reqwest::Client::new();
        retry(ExponentialBackoff::default(), || async {
            let res = gvpn_client::register(&client, &input).await?;
            Ok(res)
        })
        .await
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

    async fn wg_tunnel(
        &self,
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

    async fn ping(&self) -> Result<(), ping::Error> {
        ping::ping(&self.options.ping_options)
    }

    async fn adjust_to_main_session(&self, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
        let active_client = match session_client_metadata.active_clients.as_slice() {
            [] => return Err(HoprError::SessionNotFound),
            [client] => client.clone(),
            _ => return Err(HoprError::SessionAmbiguousClient),
        };
        let surb_management =
            to_surb_balancer_config(self.options.buffer_sizes.main, self.options.max_surb_upstream.main);
        self.hoprd.adjust_session(surb_management, active_client).await
    }
}

impl Display for ConnectionRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionRunner {{ conn: {} }}", self.conn)
    }
}

impl Display for Evt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Evt::OpenBridge => write!(f, "OpenBridge"),
            Evt::Register(pk) => write!(f, "Register({})", pk),
            Evt::CloseBridge => write!(f, "CloseBridge"),
            Evt::OpenPing => write!(f, "OpenPing"),
            Evt::WgTunnel => write!(f, "WgTunnel"),
            Evt::Ping => write!(f, "Ping"),
            Evt::AdjustToMain => write!(f, "AdjustToMain"),
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
