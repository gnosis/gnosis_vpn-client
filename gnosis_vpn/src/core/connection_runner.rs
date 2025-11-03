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
    Ping(Uuid),
    AdjustMain(Uuid),
    TunnelWg(Uuid),
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
        evt_sender.send(Evt::OpenBridge(self.id)).await;
        let session_client_metadata = self.open_bridge_session().await?;
        evt_sender.send(Evt::Register(self.id)).await;
        let registration = self.register(&session_client_metadata).await?;
        evt_sender.send(Evt::CloseBridge(self.id)).await;
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
}
