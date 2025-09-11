use reqwest::{StatusCode, blocking};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp;
use std::fmt::{self, Display};
use std::net::SocketAddr;
use thiserror::Error;

use crate::address::Address;
use crate::entry_node::EntryNode;
use crate::remote_data;

pub use path::Path;
pub use protocol::Protocol;

mod path;
mod protocol;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub active_clients: Vec<String>,
    pub destination: Address,
    pub forward_path: Path,
    pub ip: String,
    pub hopr_mtu: u16,
    pub port: u16,
    pub protocol: Protocol,
    pub return_path: Path,
    pub surb_len: u16,
    pub target: String,
    pub max_client_sessions: u16,
    pub max_surb_upstream: String,
    pub response_buffer: String,
    pub session_pool: Option<u16>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub enum Capability {
    NoDelay,
    NoRateControl,
    Retransmission,
    RetransmissionAckOnly,
    Segmentation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Plain(SocketAddr),
    Sealed(SocketAddr),
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
    entry_node: EntryNode,
    destination: Address,
    capabilities: Vec<Capability>,
    path: Path,
    target: Target,
    protocol: Protocol,
    // https://docs.rs/bytesize/2.0.1/bytesize/ string
    response_buffer: String,
    // https://docs.rs/human-bandwidth/0.1.4/human_bandwidth/ string
    max_surb_upstream: String,
}

pub struct CloseSession {
    entry_node: EntryNode,
}

pub struct ListSession {
    entry_node: EntryNode,
    protocol: Protocol,
}

pub struct UpdateSessionConfig {
    entry_node: EntryNode,
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

impl Target {
    pub fn plain(addr: &SocketAddr) -> Self {
        Target::Plain(*addr)
    }

    pub fn sealed(addr: &SocketAddr) -> Self {
        Target::Sealed(*addr)
    }

    pub fn type_(&self) -> String {
        match self {
            Target::Plain(_) => "Plain".to_string(),
            Target::Sealed(_) => "Sealed".to_string(),
        }
    }

    pub fn address(&self) -> String {
        match self {
            Target::Plain(addr) => addr.to_string(),
            Target::Sealed(addr) => addr.to_string(),
        }
    }
}

impl OpenSession {
    pub fn bridge(
        entry_node: EntryNode,
        destination: Address,
        capabilities: Vec<Capability>,
        path: Path,
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
        entry_node: EntryNode,
        destination: Address,
        capabilities: Vec<Capability>,
        path: Path,
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

impl CloseSession {
    pub fn new(entry_node: &EntryNode) -> Self {
        CloseSession {
            entry_node: entry_node.clone(),
        }
    }
}

impl ListSession {
    pub fn new(entry_node: &EntryNode, protocol: &Protocol) -> Self {
        ListSession {
            entry_node: entry_node.clone(),
            protocol: protocol.clone(),
        }
    }
}

impl UpdateSessionConfig {
    pub fn new(entry_node: &EntryNode, response_buffer: String, max_surb_upstream: String) -> Self {
        UpdateSessionConfig {
            entry_node: entry_node.clone(),
            response_buffer,
            max_surb_upstream,
        }
    }
}

impl Session {
    pub fn open(client: &blocking::Client, open_session: &OpenSession) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(open_session.entry_node.api_token.as_str())?;
        let path = format!(
            "api/{}/session/{}",
            open_session.entry_node.api_version, open_session.protocol
        );
        let url = open_session.entry_node.endpoint.join(&path)?;

        let mut json = serde_json::Map::new();
        json.insert("destination".to_string(), json!(open_session.destination));

        let target = open_session.target.clone();
        let target_json = json!({ target.type_(): target.address() });
        json.insert("target".to_string(), target_json);

        let path_json = match open_session.path.clone() {
            Path::Hops(hop) => {
                json!({"Hops": hop})
            }
            Path::IntermediatePath(addresses) => {
                json!({ "IntermediatePath": addresses.clone() })
            }
        };
        json.insert("forwardPath".to_string(), path_json.clone());
        json.insert("returnPath".to_string(), path_json);
        json.insert("listenHost".to_string(), json!(&open_session.entry_node.listen_host));

        json.insert("capabilities".to_string(), json!(open_session.capabilities));
        json.insert("responseBuffer".to_string(), json!(open_session.response_buffer));
        json.insert("maxSurbUpstream".to_string(), json!(open_session.max_surb_upstream));
        // creates a TCP session as part of the session pool, so we immediately know if it might work
        if open_session.protocol == Protocol::Tcp {
            json.insert("sessionPool".to_string(), json!(1));
        }

        tracing::debug!(?headers, body = ?json, %url, "post open session");
        let resp = client
            .post(url)
            .json(&json)
            .timeout(open_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error can only be mapped after sending
            .map_err(open_response_errors)?
            .json::<Self>()?;
        Ok(resp)
    }

    pub fn close(&self, client: &blocking::Client, close_session: &CloseSession) -> Result<(), Error> {
        let headers = remote_data::authentication_headers(close_session.entry_node.api_token.as_str())?;
        let path = format!(
            "api/{}/session/{}/{}/{}",
            close_session.entry_node.api_version, self.protocol, self.ip, self.port
        );
        let url = close_session.entry_node.endpoint.join(&path)?;

        tracing::debug!(?headers, %url, "delete session");
        client
            .delete(url)
            .timeout(close_session.entry_node.http_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(close_response_errors)?;
        Ok(())
    }

    pub fn list(client: &blocking::Client, list_session: &ListSession) -> Result<Vec<Self>, Error> {
        let headers = remote_data::authentication_headers(list_session.entry_node.api_token.as_str())?;
        let path = format!(
            "api/{}/session/{}",
            list_session.entry_node.api_version, list_session.protocol
        );
        let url = list_session.entry_node.endpoint.join(&path)?;

        tracing::debug!(?headers, %url, "list sessions");

        let resp = client
            .get(url)
            .timeout(list_session.entry_node.http_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(remote_data::response_errors)?
            .json::<Vec<Session>>()?;

        Ok(resp)
    }

    pub fn update(&self, client: &blocking::Client, config: &UpdateSessionConfig) -> Result<(), Error> {
        let active_client = match &self.active_clients.as_slice() {
            [] => return Err(Error::NoSessionId),
            [client] => client,
            _ => return Err(Error::AmbiguousSessionId),
        };
        let headers = remote_data::authentication_headers(config.entry_node.api_token.as_str())?;
        let path = format!("api/{}/session/config/{}", config.entry_node.api_version, active_client);
        let url = config.entry_node.endpoint.join(&path)?;

        let mut json = serde_json::Map::new();
        json.insert("maxSurbUpstream".to_string(), json!(config.max_surb_upstream));
        json.insert("responseBuffer".to_string(), json!(config.response_buffer));

        tracing::debug!(?headers, body = ?json, %url, "post config");

        client
            .post(url)
            .json(&json)
            .timeout(config.entry_node.http_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(remote_data::connect_errors)?
            .error_for_status()
            // response error can only be mapped after sending
            .map_err(remote_data::response_errors)?;
        Ok(())
    }

    pub fn verify_open(&self, sessions: &[Session]) -> bool {
        sessions.iter().any(|entry| entry == self)
    }
}

fn open_response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(StatusCode::CONFLICT) {
        Error::ListenHostAlreadyUsed
    } else {
        remote_data::response_errors(err).into()
    }
}

fn close_response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(StatusCode::NOT_FOUND) {
        Error::SessionNotFound
    } else {
        remote_data::response_errors(err).into()
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
