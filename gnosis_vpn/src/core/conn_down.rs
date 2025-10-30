use bytesize::ByteSize;
use edgli::hopr_lib::{SessionClientConfig, SurbBalancerConfig};
use human_bandwidth::re::bandwidth::Bandwidth;

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::connection::options::Options;

use crate::core::hopr_runner::Cmd;

#[derive(Debug, Clone)]
pub struct ConnDown {
    destination: Destination,
    options: Options,
    phase: Phase,
}

#[derive(Debug, Clone)]
enum Phase {
    Init,
}

impl ConnDown {
    pub fn new(destination: Destination, options: Options) -> Self {
        Conn {
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
            surb_management: Some(to_surb_balancer_config(
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
