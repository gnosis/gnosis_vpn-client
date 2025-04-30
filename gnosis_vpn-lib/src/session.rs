use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use std::net::SocketAddr;
use thiserror::Error;
use url::Url;

use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::remote_data;

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub target: String,
}

pub enum Capability {
    Segmentation,
    Retransmission,
}

#[derive(Clone)]
pub enum Path {
    Hop(u8),
    Intermediates(Vec<PeerId>),
}

#[derive(Clone)]
pub enum Target {
    Plain(SocketAddr),
    Sealed(SocketAddr),
}

pub enum Protocol {
    Udp,
    Tcp,
}

pub struct OpenSession {
    entry_node: EntryNode,
    destination: String,
    capabilities: Option<Vec<Capability>>,
    path: Option<Path>,
    target: Option<Target>,
    protocol: Protocol,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Udp => write!(f, "udp"),
            Protocol::Tcp => write!(f, "tcp"),
        }
    }
}

impl OpenSession {
    pub fn bridge(entry_node: &EntryNode, destination: &str, path: &Option<Path>, target: &Option<Target>) -> Self {
        OpenSession {
            entry_node: entry_node.clone(),
            destination: destination.to_string(),
            capabilities: Some(vec![Capability::Segmentation]),
            path: path.clone(),
            target: target.clone(),
            protocol: Protocol::Tcp,
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid header")]
    Header(#[from] remote_data::HeaderError),
    #[error("Error parsing url")]
    Url(#[from] url::ParseError),
}

pub fn open(open_session: &OpenSession) -> Result<Session, Error> {
    let headers =
        remote_data::authentication_headers(open_session.entry_node.api_token.as_str()).map_err(Error::Header)?;
    let url = open_session
        .entry_node
        .endpoint
        .join("api/v3/session/")?
        .join(open_session.protocol.as_str())?;
    let mut json = serde_json::Map::new();
    json.insert("destination".to_string(), json!(open_session.destination));

    let target = open_session.target.clone().unwrap_or_default();
    let target_type = target.type_.unwrap_or_default();
    let target_host = target.host.unwrap_or(config::default_session_target_host());
    let target_port = target.port.unwrap_or(config::default_session_target_port());

    let target_json = json!({ target_type.to_string(): format!("{}:{}", target_host, target_port) });
    json.insert("target".to_string(), target_json);
    let path_json = match open_session.path.clone() {
        Some(SessionPathConfig::Hop(hop)) => {
            json!({"Hops": hop})
        }
        Some(SessionPathConfig::Intermediates(ids)) => {
            json!({ "IntermediatePath": ids.clone() })
        }
        None => {
            json!({"Hops": 1})
        }
    };

    json.insert("path".to_string(), path_json);
    if let Some(lh) = &open_session.listen_host {
        json.insert("listenHost".to_string(), json!(lh));
    };

    let capabilities_json = match &open_session.capabilities {
        Some(caps) => {
            json!(caps)
        }
        None => {
            json!(["Segmentation"])
        }
    };
    json.insert("capabilities".to_string(), capabilities_json);

    tracing::debug!(?headers, body = ?json, ?url, "post open session");
    let fetch_res = client
        .post(url)
        .json(&json)
        .timeout(std::time::Duration::from_secs(30))
        .headers(headers)
        .send()
        .map(|res| (res.status(), res.json::<serde_json::Value>()));

    match fetch_res {
        Ok((status, Ok(json))) if status.is_success() => {
            let session = serde_json::from_value::<Session>(value)?;
            Ok(session)
        }
        Ok((status, Ok(json))) => {
            let e = remote_data::CustomError {
                reqw_err: None,
                status: Some(status),
                value: Some(json),
            };
            Err(e)
        }
        Ok((status, Err(e))) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: Some(status),
                value: None,
            };
            Err(e)
        }
        Err(e) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: None,
                value: None,
            };
            Err(e)
        }
    }
}
