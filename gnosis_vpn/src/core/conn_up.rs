use edgli::hopr_lib::{SessionClientConfig, SessionClientMetadata};

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::connection::options::Options;
use gnosis_vpn_lib::hopr::types::SessionClientMetadata;

use crate::core::conn;
use crate::core::hopr_runner::{self, Cmd};

#[derive(Debug, Clone)]
pub struct ConnUp {
    destination: Destination,
    options: Options,
    phase: Phase,
}

#[derive(Debug, Clone)]
enum Phase {
    Init,
}

impl ConnUp {
    pub fn new(destination: Destination, options: Options) -> Self {
        ConnUp {
            destination,
            phase: Phase::Init,
            options,
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
            destination: self.destination.address,
            target: self.options.sessions.bridge.target.clone(),
            session_pool: Some(1),
            max_client_sessions: Some(1),
            cfg,
        }
    }

    pub fn on_open_session_result(&mut self, res: Result<SessionClientMetadata, hopr_runner::Error>) {}
}
