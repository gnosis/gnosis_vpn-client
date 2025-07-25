use reqwest::{StatusCode, blocking};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp;
use std::fmt::{self, Display};
use std::net::SocketAddr;
use thiserror::Error;

use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::remote_data;

pub use path::Path;
pub use protocol::Protocol;

mod path;
mod protocol;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub ip: String,
    pub port: u16,
    pub protocol: Protocol,
    pub target: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub enum Capability {
    Segmentation,
    Retransmission,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Plain(SocketAddr),
    Sealed(SocketAddr),
}

pub struct OpenSession {
    entry_node: EntryNode,
    destination: PeerId,
    capabilities: Vec<Capability>,
    path: Path,
    target: Target,
    protocol: Protocol,
}

pub struct CloseSession {
    entry_node: EntryNode,
}

pub struct ListSession {
    entry_node: EntryNode,
    protocol: Protocol,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid header: {0}")]
    Header(#[from] remote_data::HeaderError),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Session listen host already used")]
    ListenHostAlreadyUsed,
    #[error("Session not found")]
    SessionNotFound,
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Error connecting on specified port: {0:?}")]
    SocketConnect(reqwest::Error),
    #[error("Timeout: {0:?}")]
    Timeout(reqwest::Error),
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
        destination: PeerId,
        capabilities: Vec<Capability>,
        path: Path,
        target: Target,
    ) -> Self {
        OpenSession {
            entry_node: entry_node.clone(),
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            protocol: Protocol::Tcp,
        }
    }

    pub fn main(
        entry_node: EntryNode,
        destination: PeerId,
        capabilities: Vec<Capability>,
        path: Path,
        target: Target,
    ) -> Self {
        OpenSession {
            entry_node: entry_node.clone(),
            destination,
            capabilities,
            path: path.clone(),
            target: target.clone(),
            protocol: Protocol::Udp,
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

impl Session {
    pub fn open(client: &blocking::Client, open_session: &OpenSession) -> Result<Self, Error> {
        let headers = remote_data::authentication_headers(open_session.entry_node.api_token.as_str())?;
        let url = open_session
            .entry_node
            .endpoint
            .join("api/v3/session/")?
            .join(open_session.protocol.to_string().as_str())?;
        let mut json = serde_json::Map::new();
        json.insert("destination".to_string(), json!(open_session.destination));

        let target = open_session.target.clone();
        let target_json = json!({ target.type_(): target.address() });
        json.insert("target".to_string(), target_json);

        let path_json = match open_session.path.clone() {
            Path::Hops(hop) => {
                json!({"Hops": hop})
            }
            Path::IntermediatePath(ids) => {
                json!({ "IntermediatePath": ids.clone() })
            }
        };
        json.insert("path".to_string(), path_json);
        json.insert("listenHost".to_string(), json!(&open_session.entry_node.listen_host));

        json.insert("capabilities".to_string(), json!(open_session.capabilities));

        tracing::debug!(?headers, body = ?json, %url, "post open session");
        let resp = client
            .post(url)
            .json(&json)
            .timeout(open_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(connect_errors)?
            .error_for_status()
            // response error can only be mapped after sending
            .map_err(open_response_errors)?
            .json::<Self>()?;
        Ok(resp)
    }

    pub fn close(&self, client: &blocking::Client, close_session: &CloseSession) -> Result<(), Error> {
        let headers = remote_data::authentication_headers(close_session.entry_node.api_token.as_str())?;
        let path = format!("api/v3/session/{}/{}/{}", self.protocol, self.ip, self.port);
        let url = close_session.entry_node.endpoint.join(path.as_str())?;

        tracing::debug!(?headers, %url, "delete session");
        client
            .delete(url)
            .timeout(close_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(close_response_errors)?;
        Ok(())
    }

    pub fn list(client: &blocking::Client, list_session: &ListSession) -> Result<Vec<Session>, Error> {
        let headers = remote_data::authentication_headers(list_session.entry_node.api_token.as_str())?;
        let path = format!("api/v3/session/{}", list_session.protocol);
        let url = list_session.entry_node.endpoint.join(path.as_str())?;

        tracing::debug!(?headers, %url, "list sessions");

        let resp = client
            .get(url)
            .timeout(list_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            // connection error checks happen before response
            .map_err(connect_errors)?
            .error_for_status()
            // response error checks happen after response
            .map_err(response_errors)?
            .json::<Vec<Session>>()?;

        Ok(resp)
    }

    pub fn verify_open(&self, sessions: &[Session]) -> bool {
        sessions.iter().any(|entry| entry == self)
    }
}

fn open_response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(StatusCode::CONFLICT) {
        Error::ListenHostAlreadyUsed
    } else if err.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
        Error::Unauthorized
    } else {
        err.into()
    }
}

fn close_response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(StatusCode::NOT_FOUND) {
        Error::SessionNotFound
    } else if err.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
        Error::Unauthorized
    } else {
        err.into()
    }
}

fn response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
        Error::Unauthorized
    } else {
        err.into()
    }
}

fn connect_errors(err: reqwest::Error) -> Error {
    if err.is_connect() {
        Error::SocketConnect(err)
    } else if err.is_timeout() {
        Error::Timeout(err)
    } else {
        err.into()
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
