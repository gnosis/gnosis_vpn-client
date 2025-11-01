use edgli::hopr_lib::SessionClientConfig;
use uuid::{self, Uuid};

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;
use gnosis_vpn_lib::wg_tooling;

use crate::core::conn;
use crate::core::hopr_runner::{self, Cmd};

#[derive(Debug, Clone)]
pub struct ConnUp {
    destination: Destination,
    options: Options,
    phase: Phase,
    id: Uuid,
    wg: wg_tooling::WireGuard,
}

#[derive(Debug, Clone)]
enum Phase {
    Init,
}

impl ConnUp {
    pub fn new(destination: Destination, options: Options, wg: wg_tooling::WireGuard) -> Self {
        ConnUp {
            destination,
            phase: Phase::Init,
            options,
            id: Uuid::new_v4(),
            wg,
        }
    }

    pub fn init_cmd(&self) -> Cmd {
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
        Cmd::OpenSession {
            id: self.id,
            destination: self.destination.address,
            target: self.options.sessions.bridge.target.clone(),
            session_pool: Some(1),
            max_client_sessions: Some(1),
            cfg,
        }
    }

    pub fn on_open_session_result(&mut self, id: Uuid, res: Result<SessionClientMetadata, hopr_runner::Error>) {
        if id != self.id {
            return;
        }
    }
}
