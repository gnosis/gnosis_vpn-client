use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::connection::Destination as ConnectionDestination;
use crate::log_output;
use crate::peer_id::PeerId;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(PeerId),
    ConnectMeta((String, String)),
    Disconnect,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    ConnectMeta(ConnectResponse),
    Disconnect(DisconnectResponse),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub wireguard: WireguardStatus,
    pub status: Status,
    pub available_destinations: Vec<Destination>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WireguardStatus {
    Up,
    Down,
    ManuallyManaged,
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
    CannotConnect(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    CannotDisconnect(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub peer_id: PeerId,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = log_output::serialize(self);
        write!(f, "{}", s)
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
        }
    }
}
