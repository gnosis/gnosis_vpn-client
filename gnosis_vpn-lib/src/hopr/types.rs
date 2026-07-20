use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::exports::network::types::types::IpProtocol;
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::net::SocketAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Response body for creating a new client session.
pub struct SessionClientMetadata {
    /// Target of the Session.
    pub target: String,
    /// Destination node (exit node) of the Session.
    pub destination: Address,
    /// Forward routing path.
    pub forward_path: HopRouting,
    /// Return routing path.
    pub return_path: HopRouting,
    /// IP protocol used by Session's listening socket.
    pub protocol: IpProtocol,
    /// Bound address and port of the session.
    pub bound_host: SocketAddr,
    /// MTU used by the underlying HOPR transport.
    pub hopr_mtu: usize,
    /// Size of a Single Use Reply Block used by the protocol.
    ///
    /// This is useful for SURB balancing calculations.
    pub surb_len: usize,
    /// Lists Session IDs of all active clients.
    ///
    /// Can contain multiple entries on TCP sessions, but currently
    /// always only a single entry on UDP sessions.
    pub active_clients: Vec<String>,
    /// The maximum number of client sessions that the listener can spawn.
    ///
    /// This currently applies only to the TCP sessions, as UDP sessions cannot
    /// have multiple clients (defaults to 1 for UDP).
    pub max_client_sessions: usize,
    /// The maximum throughput at which artificial SURBs might be generated and sent
    /// to the recipient of the Session.
    pub max_surb_upstream: Option<human_bandwidth::re::bandwidth::Bandwidth>,
    /// The amount of response data the Session counterparty can deliver back to us, without us
    /// sending any SURBs to them.
    pub response_buffer: Option<bytesize::ByteSize>,
    /// How many Sessions to pool for clients.
    pub session_pool: Option<usize>,
}

impl PartialEq for SessionClientMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.destination == other.destination
            && self.forward_path == other.forward_path
            && self.return_path == other.return_path
            && self.protocol == other.protocol
            && self.bound_host == other.bound_host
    }
}

impl Display for SessionClientMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SessionClientMetadata {{ target: {}, destination: {}, protocol: {:?}, bound_host: {} }}",
            self.target, self.destination, self.protocol, self.bound_host
        )
    }
}

/// A raw HOPR session opened for the spliced WireGuard data plane.
///
/// Unlike the bridge/registration sessions opened through the local listener
/// bridge, this session is not tracked in `open_listeners`: it has no bound socket
/// (`metadata.bound_host` is a vestigial placeholder) and closing it means dropping
/// `session`. The `configurator` is the direct handle for SURB balancer adjustments.
pub struct SplicedWgSession {
    pub session: edgli::hopr_lib::exports::transport::HoprSession,
    pub configurator: edgli::hopr_lib::exports::transport::HoprSessionConfigurator,
    pub metadata: SessionClientMetadata,
}
