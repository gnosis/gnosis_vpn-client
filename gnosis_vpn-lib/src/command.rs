use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::address::Address;
use crate::connection::destination::Destination as ConnectionDestination;
use crate::log_output;
use crate::session;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(Address),
    Disconnect,
    Ping,
    Balance,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(BalanceResponse),
    Pong,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: Status,
    pub available_destinations: Vec<Destination>,
    pub funding_state: FundingState, // top prio funding state
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Status {
    Connecting(Destination),
    Disconnecting(Destination),
    Connected(Destination),
    Disconnected,
}

// in order of priority
#[derive(Debug, Serialize, Deserialize)]
pub enum FundingState {
    Unfunded,           // cannot work at all - initial state
    ChannelsOutOfFunds, // does not work - no traffic possible
    SafeOutOfFunds,     // keeps working - cannot top up channels
    SafeLowOnFunds,     // warning before SafeOutOfFunds
    NodeUnderfunded,    // keeps working - cannot open new channels
    NodeLowOnFunds,     // warning before NodeUnderfunded
    WellFunded,         // everything is fine
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ConnectResponse {
    Connecting(Destination),
    AddressNotFound,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    NotConnected,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub address: Address,
    pub path: session::Path,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub node: String,
    pub safe: String,
    pub channels_out: String,
    pub issues: Vec<FundingState>,
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

    pub fn address_not_found() -> Self {
        ConnectResponse::AddressNotFound
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
    pub fn new(status: Status, available_destinations: Vec<Destination>, funding_state: FundingState) -> Self {
        StatusResponse {
            status,
            available_destinations,
            funding_state,
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
            address: destination.address,
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
        let short_addr = log_output::address(&self.address);
        write!(
            f,
            "Address: {address}, Route: (entry){path}({short_addr}), {meta}",
            meta = meta,
            path = self.path,
            address = self.address,
            short_addr = short_addr,
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
