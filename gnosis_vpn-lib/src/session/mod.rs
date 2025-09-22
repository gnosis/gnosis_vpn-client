use serde::{Deserialize, Serialize};
use std::cmp;
use std::fmt::{self, Display};
use std::sync::Arc;
use thiserror::Error;

use edgli::hopr_lib::{
    RoutingOptions, SessionCapabilities as Capabilities, SessionCapability as Capability, SessionTarget as Target,
};

use crate::hopr::{Hopr, HoprError};
use crate::remote_data;
use edgli::hopr_lib::Address;

pub use protocol::Protocol;

mod protocol;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub active_clients: Vec<String>,
    pub destination: Address,
    pub forward_path: RoutingOptions,
    pub ip: String,
    pub hopr_mtu: u16,
    pub port: u16,
    pub protocol: Protocol,
    pub return_path: RoutingOptions,
    pub surb_len: u16,
    pub target: String,
    pub max_client_sessions: u16,
    pub max_surb_upstream: String,
    pub response_buffer: String,
    pub session_pool: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    // https://docs.rs/human-bandwidth/0.1.4/human_bandwidth/ string
    pub max_surb_upstream: String,
    // https://docs.rs/bytesize/2.0.1/bytesize/ string
    pub response_buffer: String,
}

pub struct OpenSession {
    entry_node: Arc<Hopr>,
    destination: Address,
    capabilities: Capabilities,
    path: RoutingOptions,
    target: Target,
    protocol: Protocol,
    // https://docs.rs/bytesize/2.0.1/bytesize/ string
    response_buffer: String,
    // https://docs.rs/human-bandwidth/0.1.4/human_bandwidth/ string
    max_surb_upstream: String,
}

pub struct CloseSession {
    entry_node: Arc<Hopr>,
}

pub struct ListSession {
    entry_node: Arc<Hopr>,
    protocol: Protocol,
}

pub struct UpdateSessionConfig {
    entry_node: Arc<Hopr>,
    // https://docs.rs/bytesize/2.0.1/bytesize/ string
    response_buffer: String,
    // https://docs.rs/human-bandwidth/0.1.4/human_bandwidth/ string
    max_surb_upstream: String,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("RemoteData error: {0}")]
    RemoteData(#[from] remote_data::Error),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Session listen host already used")]
    ListenHostAlreadyUsed,
    #[error("Session not found")]
    SessionNotFound,
    #[error("Session does not have any active clients")]
    NoSessionId,
    #[error("Session has more than one active client")]
    AmbiguousSessionId,
}

impl OpenSession {
    pub fn bridge(
        entry_node: Arc<Hopr>,
        destination: Address,
        capabilities: Capabilities,
        path: RoutingOptions,
        target: Target,
        buffer_size: String,
        max_surb_upstream: String,
    ) -> Self {
        OpenSession {
            entry_node: entry_node.clone(),
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            protocol: Protocol::Tcp,
            response_buffer: buffer_size,
            max_surb_upstream,
        }
    }

    pub fn main(
        entry_node: Arc<Hopr>,
        destination: Address,
        capabilities: Capabilities,
        path: RoutingOptions,
        target: Target,
        buffer_size: String,
        max_surb_upstream: String,
    ) -> Self {
        OpenSession {
            entry_node: entry_node.clone(),
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            protocol: Protocol::Udp,
            response_buffer: buffer_size,
            max_surb_upstream,
        }
    }
}

impl From<&OpenSession> for edgli::hopr_lib::SessionClientConfig {
    fn from(open: &OpenSession) -> Self {
        Self {
            capabilities: open.capabilities,
            forward_path_options: open.path.clone(),
            return_path_options: open.path.clone(), // TODO: check this for intermediate behavior
            ..Default::default()
        }
    }
}

impl CloseSession {
    pub fn new(entry_node: &Arc<Hopr>) -> Self {
        CloseSession {
            entry_node: entry_node.clone(),
        }
    }
}

impl ListSession {
    pub fn new(entry_node: &Arc<Hopr>, protocol: &Protocol) -> Self {
        ListSession {
            entry_node: entry_node.clone(),
            protocol: protocol.clone(),
        }
    }
}

impl UpdateSessionConfig {
    pub fn new(entry_node: &Arc<Hopr>, response_buffer: String, max_surb_upstream: String) -> Self {
        UpdateSessionConfig {
            entry_node: entry_node.clone(),
            response_buffer,
            max_surb_upstream,
        }
    }
}

impl Session {
    /// TODO: implement
    pub fn open(open_session: &OpenSession) -> Result<Self, HoprError> {
        let r = open_session.entry_node.open_session(
            open_session.destination,
            open_session.target.clone(),
            open_session.into(),
        )?;

        unimplemented!(); // TODO: fill in fields from `r` --- IGNORE ---
    }

    /// TODO: implement
    pub fn close(&self, close_session: &CloseSession) -> Result<(), HoprError> {
        close_session.entry_node.close_session(0)?; // TODO: pass session id
        Ok(())
    }

    /// TODO: implement
    pub fn list(list_session: &ListSession) -> Result<Vec<Self>, HoprError> {
        list_session.entry_node.list_sessions(list_session.protocol)?;
        unimplemented!()
    }

    /// TODO: implement
    pub fn update(&self, config: &UpdateSessionConfig) -> Result<(), HoprError> {
        // let active_client = match &self.active_clients.as_slice() {
        //     [] => return Err(Error::NoSessionId),
        //     [client] => client,
        //     _ => return Err(Error::AmbiguousSessionId),
        // };       // TODO: handle these specifically

        let active_client = "".to_string(); // TODO: fix this

        config.entry_node.update_session(
            config.max_surb_upstream.clone(),
            config.response_buffer.clone(),
            active_client.clone(),
        )?;
        Ok(())
    }

    pub fn verify_open(&self, sessions: &[Session]) -> bool {
        sessions.iter().any(|entry| entry == self)
    }
}

impl cmp::PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.ip == other.ip && self.port == other.port && self.protocol == other.protocol && self.target == other.target
    }
}

impl Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Session[{}/{}]", self.port, self.protocol)
    }
}
