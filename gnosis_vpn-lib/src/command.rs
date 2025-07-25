use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::connection::Destination as ConnectionDestination;
use crate::log_output;
use crate::peer_id::PeerId;
use crate::session;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(PeerId),
    Disconnect,
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Pong,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: Status,
    pub available_destinations: Vec<Destination>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Status {
    Connecting(Destination),
    Disconnecting(Destination),
    Connected(Destination),
    Disconnected,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ConnectResponse {
    Connecting(Destination),
    PeerIdNotFound,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    NotConnected,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub peer_id: PeerId,
    pub path: session::Path,
}

impl Status {
    pub fn connecting(destination: Destination) -> Self {
        Status::Connecting(destination)
    }

    pub fn connected(destination: Destination) -> Self {
        Status::Connected(destination)
    }

    pub fn disconnecting(destination: Destination) -> Self {
        Status::Disconnecting(destination)
    }

    pub fn disconnected() -> Self {
        Status::Disconnected
    }
}

impl ConnectResponse {
    pub fn new(destination: Destination) -> Self {
        ConnectResponse::Connecting(destination)
    }

    pub fn peer_id_not_found() -> Self {
        ConnectResponse::PeerIdNotFound
    }
}

impl DisconnectResponse {
    pub fn new(destination: Destination) -> Self {
        DisconnectResponse::Disconnecting(destination)
    }

    pub fn not_connected() -> Self {
        DisconnectResponse::NotConnected
    }
}

impl StatusResponse {
    pub fn new(status: Status, available_destinations: Vec<Destination>) -> Self {
        StatusResponse {
            status,
            available_destinations,
        }
    }
}

impl Response {
    pub fn connect(conn: ConnectResponse) -> Self {
        Response::Connect(conn)
    }

    pub fn disconnect(disc: DisconnectResponse) -> Self {
        Response::Disconnect(disc)
    }

    pub fn status(stat: StatusResponse) -> Self {
        Response::Status(stat)
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = log_output::serialize(self);
        write!(f, "{s}")
    }
}

impl FromStr for Command {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl From<ConnectionDestination> for Destination {
    fn from(destination: ConnectionDestination) -> Self {
        Destination {
            peer_id: destination.peer_id,
            meta: destination.meta,
            path: destination.path,
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let meta = self
            .meta
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ");
        let short_pid = log_output::peer_id(self.peer_id.to_string().as_str());
        write!(
            f,
            "Peer ID: {pid}, Route: (entry){path}(x{short_pid}), {meta}",
            meta = meta,
            path = self.path,
            pid = self.peer_id,
            short_pid = short_pid,
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Status::Connecting(dest) => write!(f, "Connecting to {dest}"),
            Status::Disconnecting(dest) => write!(f, "Disconnecting from {dest}"),
            Status::Connected(dest) => write!(f, "Connected to {dest}"),
            Status::Disconnected => write!(f, "Disconnected"),
        }
    }
}
