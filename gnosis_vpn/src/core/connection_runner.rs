use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::{self, Uuid};

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client::{self, Registration};
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::hopr::{Hopr, HoprError};
use gnosis_vpn_lib::wg_tooling;

use crate::core::conn;

use crate::hopr_params::{self, HoprParams};
use std::sync::Arc;

pub struct ConnectionRunner {
    hoprd: Arc<Hopr>,
    id: Uuid,
    destination: Destination,
    options: Options,
    wg: wg_tooling::WireGuard,
}

#[derive(Debug)]
pub enum Evt {
    OpenBridge(Uuid),
    Register(Uuid),
    CloseBridge(Uuid),
    OpenPing(Uuid),
    WgTunnel(Uuid),
    Ping(Uuid),
    AdjustToMain(Uuid),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Hopr(#[from] HoprError),
    #[error(transparent)]
    GvpnClient(#[from] gvpn_client::Error),
}

impl ConnectionRunner {
    pub fn new(destination: Destination, options: Options, wg: wg_tooling::WireGuard, hoprd: Arc<Hopr>) -> Self {
        Self {
            hoprd,
            id: Uuid::new_v4(),
            destination,
            options,
            wg,
        }
    }

    pub async fn connect(&self, evt_sender: mpsc::Sender<Evt>) -> Result<(), Error> {
        // 1. open bridge session
        evt_sender.send(Evt::OpenBridge(self.id)).await;
        let bridge_session = self.open_bridge_session().await?;

        // 2. register wg public key
        evt_sender.send(Evt::Register(self.id)).await;
        let registration = self.register(&bridge_session).await?;

        // 3. close bridge session
        evt_sender.send(Evt::CloseBridge(self.id)).await;
        self.close_bridge_session(&bridge_session).await?;

        // 4. open ping session
        evt_sender.send(Evt::OpenPing(self.id)).await;
        let ping_session = self.open_ping_session().await?;

        // 5. setup wg tunnel
        evt_sender.send(Evt::WgTunnel(self.id)).await;
        self.wg_tunnel(&registration, &ping_session).await?;

        // 6. check ping
        evt_sender.send(Evt::Ping(self.id)).await;
        self.ping(&ping_session, &registration).await?;

        // 7. adjust to main session
        evt_sender.send(Evt::AdjustToMain(self.id)).await;
        self.adjust_to_main_session(&ping_session).await?;

        Ok(())
    }

    async fn open_bridge_session(&self) -> Result<SessionClientMetadata, HoprError> {
        let cfg = SessionClientConfig {
            capabilities: self.options.sessions.bridge.capabilities,
            forward_path_options: self.destination.routing.clone(),
            return_path_options: self.destination.routing.clone(),
            surb_management: Some(conn::to_surb_balancer_config(
                self.options.buffer_sizes.bridge,
                self.options.max_surb_upstream.bridge,
            )),
            ..Default::default()
        };
        retry(ExponentialBackoff::default(), || async {
            let res = self
                .hoprd
                .open_session(
                    self.destination.address,
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
    ) -> Result<Registration, gvpn_client::Error> {
        let input = gvpn_client::Input::new(
            self.wg.key_pair.public_key.clone(),
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
            forward_path_options: self.destination.routing.clone(),
            return_path_options: self.destination.routing.clone(),
            surb_management: Some(conn::to_surb_balancer_config(
                self.options.buffer_sizes.ping,
                self.options.max_surb_upstream.ping,
            )),
            ..Default::default()
        };
        retry(ExponentialBackoff::default(), || async {
            let res = self
                .hoprd
                .open_session(
                    self.destination.address,
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
    ) -> Result<(), wg_tooling::Error> {
        // run wg-quick down once to ensure no dangling state
        _ = self.wg.close_session().await;

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
        let opts = self.options.ping_options.clone();
        let timeout = self.options.timeouts.ping_retries;
        ping::ping(&opts)
    }

    async fn adjust_to_main_session(&self, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
        let active_client = match session_client_metadata.active_clients.as_slice() {
            [] => return Err(HoprError::SessionNotFound),
            [client] => client.clone(),
            _ => return Err(HoprError::SessionAmbiguousClient),
        };
        let surb_management =
            conn::to_surb_balancer_config(self.options.buffer_sizes.main, self.options.max_surb_upstream.main);
        self.hoprd.adjust_session(surb_management, active_client).await
    }
}
