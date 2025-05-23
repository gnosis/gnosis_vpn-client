use reqwest::{blocking, StatusCode};
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
    #[error("Invalid header")]
    Header(#[from] remote_data::HeaderError),
    #[error("Error parsing url")]
    Url(#[from] url::ParseError),
    #[error("Error converting json to struct")]
    Deserialize(#[from] serde_json::Error),
    #[error("Error making http request")]
    RemoteData(remote_data::CustomError),
    #[error("Session listen host already used")]
    ListenHostAlreadyUsed,
    #[error("Session not found")]
    SessionNotFound,
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
    pub fn new(
        entry_node: EntryNode,
        destination: PeerId,
        capabilities: Vec<Capability>,
        path: Path,
        target: Target,
        protocol: Protocol,
    ) -> Self {
        OpenSession {
            entry_node,
            destination,
            capabilities,
            path,
            target,
            protocol,
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
            Path::Intermediates(ids) => {
                json!({ "IntermediatePath": ids.clone() })
            }
        };
        json.insert("path".to_string(), path_json);
        json.insert("listenHost".to_string(), json!(&open_session.entry_node.listen_host));

        json.insert("capabilities".to_string(), json!(open_session.capabilities));

        tracing::debug!(?headers, body = ?json, %url, "post open session");
        let fetch_res = client
            .post(url)
            .json(&json)
            .timeout(open_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            .map(|res| (res.status(), res.json::<serde_json::Value>()));

        match fetch_res {
            Ok((status, Ok(json))) if status.is_success() => {
                let session = serde_json::from_value::<Self>(json)?;
                Ok(session)
            }
            // Ok((409, Ok(Object {"status": String("LISTEN_HOST_ALREADY_USED")})))
            Ok((status, _)) if status == StatusCode::CONFLICT => Err(Error::ListenHostAlreadyUsed),
            Ok((status, Ok(json))) => {
                let e = remote_data::CustomError {
                    reqw_err: None,
                    status: Some(status),
                    value: Some(json),
                };
                Err(Error::RemoteData(e))
            }
            Ok((status, Err(e))) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: Some(status),
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
            Err(e) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: None,
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
        }
    }

    pub fn close(&self, client: &blocking::Client, close_session: &CloseSession) -> Result<(), Error> {
        let headers = remote_data::authentication_headers(close_session.entry_node.api_token.as_str())?;
        let path = format!("api/v3/session/{}/{}/{}", self.protocol, self.ip, self.port);
        let url = close_session.entry_node.endpoint.join(path.as_str())?;

        tracing::debug!(?headers, %url, "delete session");
        let fetch_res = client
            .delete(url)
            .timeout(close_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            .map(|res| (res.status(), res.json::<serde_json::Value>()));

        match fetch_res {
            Ok((status, _)) if status.is_success() => Ok(()),
            // Ok((404, Ok(Object {"status": String("INVALID_INPUT")})))
            Ok((status, _)) if status == StatusCode::NOT_FOUND => Err(Error::SessionNotFound),
            Ok((status, Ok(json))) => {
                let e = remote_data::CustomError {
                    reqw_err: None,
                    status: Some(status),
                    value: Some(json),
                };
                Err(Error::RemoteData(e))
            }
            Ok((status, Err(e))) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: Some(status),
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
            Err(e) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: None,
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
        }
    }

    pub fn list(client: &blocking::Client, list_session: &ListSession) -> Result<Vec<Session>, Error> {
        let headers = remote_data::authentication_headers(list_session.entry_node.api_token.as_str())?;
        let path = format!("api/v3/session/{}", list_session.protocol);
        let url = list_session.entry_node.endpoint.join(path.as_str())?;

        tracing::debug!(?headers, %url, "list sessions");

        let fetch_res = client
            .get(url)
            .timeout(list_session.entry_node.session_timeout)
            .headers(headers)
            .send()
            .map(|res| (res.status(), res.json::<serde_json::Value>()));

        match fetch_res {
            Ok((status, Ok(json))) if status.is_success() => {
                let sessions = serde_json::from_value::<Vec<Session>>(json)?;
                Ok(sessions)
            }
            Ok((status, Ok(json))) => {
                let e = remote_data::CustomError {
                    reqw_err: None,
                    status: Some(status),
                    value: Some(json),
                };
                Err(Error::RemoteData(e))
            }
            Ok((status, Err(e))) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: Some(status),
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
            Err(e) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: None,
                    value: None,
                };
                Err(Error::RemoteData(e))
            }
        }
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
