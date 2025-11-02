use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::SessionClientConfig;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use uuid::{self, Uuid};

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::gvpn_client::{self, Registration};
use gnosis_vpn_lib::hopr::Hopr;
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::wg_tooling;

use crate::core::conn;
use crate::core::hopr_runner::{Cmd, Evt as HoprEvt};

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
    OpeningSession(Uuid),
}

#[derive(Debug, Error)]
pub enum Error {}

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

    pub async fn start(&self, evt_sender: mpsc::Sender<Evt>) -> Result<(), Error> {
        evt_sender.send(Evt::OpeningSession(self.id)).await;
        let res = self.open_bridge_session().await;
        tracing::debug!("ConnectionRunner started");
        unimplemented!()
    }

    async fn open_bridge_session(&self) {
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
        let res = self
            .hoprd
            .open_session(
                self.destination.address,
                self.options.sessions.bridge.target.clone(),
                Some(1),
                Some(1),
                cfg,
            )
            .await;
        tracing::info!(res = ?res, "Opened bridge session");
    }
}
