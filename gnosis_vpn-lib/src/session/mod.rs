use bytesize::ByteSize;
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Serialize};
use std::cmp;
use std::fmt::{self, Display};
use std::sync::Arc;

use edgli::hopr_lib::{
    IpProtocol, RoutingOptions, SessionCapabilities as Capabilities, SessionTarget as Target, SurbBalancerConfig,
};

use crate::hopr::{Hopr, HoprError};
use edgli::hopr_lib::Address;

pub use protocol::Protocol;

mod protocol;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub active_clients: Vec<String>,
    pub destination: Address,
    pub forward_path: RoutingOptions,
    pub bound_host: std::net::SocketAddr,
    pub hopr_mtu: u16,
    pub protocol: IpProtocol,
    pub return_path: RoutingOptions,
    pub surb_len: u16,
    pub target: String,
    pub max_client_sessions: u16,
    pub max_surb_upstream: String,
    pub response_buffer: String,
    pub session_pool: Option<u16>,
}

pub struct OpenSession {
    edgli: Arc<Hopr>,
    destination: Address,
    capabilities: Capabilities,
    path: RoutingOptions,
    target: Target,
    balancer_cfg: SurbBalancerConfig,
    session_pool: Option<u16>,
    max_client_sessions: Option<u16>,
}

pub struct CloseSession {
    edgli: Arc<Hopr>,
}

pub struct ListSession {
    edgli: Arc<Hopr>,
    protocol: IpProtocol,
}

pub struct UpdateSessionConfig {
    edgli: Arc<Hopr>,
    balancer_config: SurbBalancerConfig,
}

impl OpenSession {
    // This is the TCP session used to register our wg public key at GnosisVPN server.
    pub fn bridge(
        edgli: Arc<Hopr>,
        destination: Address,
        capabilities: Capabilities,
        path: RoutingOptions,
        target: Target,
        balancer_cfg: SurbBalancerConfig,
    ) -> Self {
        OpenSession {
            edgli,
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            balancer_cfg,
            session_pool: Some(1),
            max_client_sessions: Some(1),
        }
    }

    // This is the UDP session used to send/receive WireGuard traffic via HOPR network.
    pub fn main(
        edgli: Arc<Hopr>,
        destination: Address,
        capabilities: Capabilities,
        path: RoutingOptions,
        target: Target,
        balancer_cfg: SurbBalancerConfig,
    ) -> Self {
        OpenSession {
            edgli,
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            balancer_cfg,
            // no relevance for UDP sessions
            session_pool: None,
            // no relevance for UDP sessions
            max_client_sessions: None,
        }
    }
}

impl From<&OpenSession> for edgli::hopr_lib::SessionClientConfig {
    fn from(open: &OpenSession) -> Self {
        Self {
            capabilities: open.capabilities,
            forward_path_options: open.path.clone(),
            return_path_options: open.path.clone(),
            surb_management: Some(open.balancer_cfg),
            ..Default::default()
        }
    }
}

impl CloseSession {
    pub fn new(edgli: Arc<Hopr>) -> Self {
        CloseSession { edgli }
    }
}

impl ListSession {
    pub fn new(edgli: Arc<Hopr>, protocol: IpProtocol) -> Self {
        ListSession { edgli, protocol }
    }
}

impl UpdateSessionConfig {
    pub fn new(edgli: Arc<Hopr>, response_buffer: ByteSize, max_surb_upstream: Bandwidth) -> Self {
        UpdateSessionConfig {
            edgli,
            balancer_config: to_surb_balancer_config(response_buffer, max_surb_upstream),
        }
    }
}

impl Session {
    pub fn open(open_session: &OpenSession) -> Result<Self, HoprError> {
        let session_client_metadata = open_session.edgli.open_session(
            open_session.destination,
            open_session.target.clone(),
            open_session.session_pool.map(|v| v as usize),
            open_session.max_client_sessions.map(|v| v as usize),
            open_session.into(),
        )?;

        Ok(Self {
            destination: session_client_metadata.destination,
            forward_path: session_client_metadata.forward_path,
            bound_host: session_client_metadata.bound_host,
            hopr_mtu: session_client_metadata.hopr_mtu as u16,
            protocol: session_client_metadata.protocol,
            return_path: session_client_metadata.return_path,
            surb_len: session_client_metadata.surb_len as u16,
            target: session_client_metadata.target,
            max_client_sessions: session_client_metadata.max_client_sessions as u16,
            max_surb_upstream: session_client_metadata
                .max_surb_upstream
                .map(|v| human_bandwidth::format_bandwidth(v).to_string())
                .unwrap_or_default(),
            response_buffer: session_client_metadata
                .response_buffer
                .map(|v| v.to_string())
                .unwrap_or_default(),
            session_pool: session_client_metadata.session_pool.map(|v| v as u16),
            active_clients: session_client_metadata.active_clients,
        })
    }

    pub fn close(&self, close_session: &CloseSession) -> Result<(), HoprError> {
        close_session.edgli.close_session(self.bound_host, self.protocol)
    }

    pub fn list(list_session: &ListSession) -> Result<Vec<Self>, HoprError> {
        Ok(list_session
            .edgli
            .list_sessions(list_session.protocol)
            .into_iter()
            .map(|session_client_metadata| Self {
                destination: session_client_metadata.destination,
                forward_path: session_client_metadata.forward_path,
                bound_host: session_client_metadata.bound_host,
                hopr_mtu: session_client_metadata.hopr_mtu as u16,
                protocol: session_client_metadata.protocol,
                return_path: session_client_metadata.return_path,
                surb_len: session_client_metadata.surb_len as u16,
                target: session_client_metadata.target,
                max_client_sessions: session_client_metadata.max_client_sessions as u16,
                max_surb_upstream: session_client_metadata
                    .max_surb_upstream
                    .map(|v| human_bandwidth::format_bandwidth(v).to_string())
                    .unwrap_or_default(),
                response_buffer: session_client_metadata
                    .response_buffer
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                session_pool: session_client_metadata.session_pool.map(|v| v as u16),
                active_clients: session_client_metadata.active_clients,
            })
            .collect())
    }

    pub fn update(&self, config: &UpdateSessionConfig) -> Result<(), HoprError> {
        let active_client = match self.active_clients.as_slice() {
            [] => return Err(HoprError::SessionNotFound),
            [client] => client.clone(),
            _ => return Err(HoprError::SessionAmbiguousClient),
        };

        config.edgli.adjust_session(config.balancer_config, active_client)
    }

    pub fn verify_open(&self, sessions: &[Session]) -> bool {
        sessions.iter().any(|entry| entry == self)
    }
}

pub(crate) fn to_surb_balancer_config(response_buffer: ByteSize, max_surb_upstream: Bandwidth) -> SurbBalancerConfig {
    if response_buffer.as_u64() >= 2 * edgli::hopr_lib::SESSION_MTU as u64 {
        SurbBalancerConfig {
            target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::SESSION_MTU as u64,
            max_surbs_per_sec: max_surb_upstream.as_bps() as u64 / (8 * edgli::hopr_lib::SURB_SIZE) as u64,
            ..Default::default()
        }
    } else {
        Default::default()
    }
}

impl cmp::PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.bound_host == other.bound_host && self.protocol == other.protocol && self.target == other.target
    }
}

impl Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Session[{}/{}]", self.bound_host.port(), self.protocol)
    }
}
