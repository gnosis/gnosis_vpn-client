use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::address::Address;
use crate::balance::FundingIssue;
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
    RefreshNode,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(Option<BalanceResponse>),
    RefreshNode,
    Pong,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: Status,
    pub available_destinations: Vec<Destination>,
    pub funding: FundingState,
    pub network: Option<String>,
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
    Unknown, // state not queried yet
    TopIssue(FundingIssue),
    WellFunded,
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
    pub addresses: Addresses,
    pub issues: Vec<FundingIssue>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Addresses {
    pub node: Address,
    pub safe: Address,
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
    pub fn new(
        status: Status,
        available_destinations: Vec<Destination>,
        funding: FundingState,
        network: Option<String>,
    ) -> Self {
        StatusResponse {
            status,
            available_destinations,
            funding,
            network,
        }
    }
}

impl BalanceResponse {
    pub fn new(
        node: String,
        safe: String,
        channels_out: String,
        issues: Vec<FundingIssue>,
        addresses: Addresses,
    ) -> Self {
        BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            addresses,
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

impl From<Option<Vec<FundingIssue>>> for FundingState {
    fn from(issues: Option<Vec<FundingIssue>>) -> Self {
        let issues = match issues {
            Some(issues) => issues,
            None => return FundingState::Unknown,
        };
        if issues.is_empty() {
            return FundingState::WellFunded;
        }
        let top_issue = &issues[0];
        FundingState::TopIssue(top_issue.clone())
    }
}
