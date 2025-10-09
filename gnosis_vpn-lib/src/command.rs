use edgli::hopr_lib::{Address, RoutingOptions};
use edgli::hopr_lib::{Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::balance::{self, FundingIssue};
use crate::connection::destination::Destination as ConnectionDestination;
use crate::log_output;
use crate::network::Network;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(Address),
    Disconnect,
    Balance,
    Ping,
    RefreshNode,
    FundingTool(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(Option<BalanceResponse>),
    Pong,
    Empty,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub run_mode: RunMode,
    pub available_destinations: Vec<Destination>,
    pub network: Network,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RunMode {
    /// Initial start
    Init,
    /// after creating safe this state will not be reached again
    PreparingSafe {
        node_address: Address,
        node_xdai: Balance<XDai>,
        node_wxhopr: Balance<WxHOPR>,
        funding_tool: balance::FundingTool,
    },
    /// Before config generation
    ValueingTicket,
    /// Subsequent service start up in this state and after preparing safe
    Warmup { sync_progress: f32, hopr_state: String },
    /// Normal operation where connections can be made
    Running {
        connection: ConnectionState,
        funding: FundingState,
        hopr_state: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ConnectionState {
    Connecting(Destination),
    Disconnecting(Destination),
    Connected(Destination),
    Disconnected,
}

// in order of priority
#[derive(Debug, Serialize, Deserialize)]
pub enum FundingState {
    Unknown,                // state not queried yet
    TopIssue(FundingIssue), // there is at least one issue
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
    pub path: RoutingOptions,
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

impl ConnectionState {
    pub fn connecting(destination: Destination) -> Self {
        ConnectionState::Connecting(destination)
    }

    pub fn disconnecting(destination: Destination) -> Self {
        ConnectionState::Disconnecting(destination)
    }

    pub fn connected(destination: Destination) -> Self {
        ConnectionState::Connected(destination)
    }

    pub fn disconnected() -> Self {
        ConnectionState::Disconnected
    }
}

impl RunMode {
    pub fn initializing() -> Self {
        RunMode::Init
    }
    pub fn preparing_safe(
        node_address: Address,
        pre_safe: balance::PreSafe,
        funding_tool: balance::FundingTool,
    ) -> Self {
        RunMode::PreparingSafe {
            node_address,
            node_xdai: pre_safe.node_xdai,
            node_wxhopr: pre_safe.node_wxhopr,
            funding_tool,
        }
    }

    pub fn warmup(sync_progress: f32, hopr_state: String) -> Self {
        RunMode::Warmup {
            sync_progress,
            hopr_state,
        }
    }

    pub fn valueing_ticket() -> Self {
        RunMode::ValueingTicket
    }

    pub fn running(connection: ConnectionState, funding: FundingState, hopr_state: String) -> Self {
        RunMode::Running {
            connection,
            funding,
            hopr_state,
        }
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
    pub fn new(run_mode: RunMode, available_destinations: Vec<Destination>, network: Network) -> Self {
        StatusResponse {
            run_mode,
            available_destinations,
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
            path: destination.routing,
        }
    }
}

impl From<Vec<FundingIssue>> for FundingState {
    fn from(issues: Vec<FundingIssue>) -> Self {
        if issues.is_empty() {
            FundingState::WellFunded
        } else {
            FundingState::TopIssue(issues[0].clone())
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
            "Address: {address}, Route: (entry){path:?}({short_addr}), {meta}",
            meta = meta,
            path = self.path,
            address = self.address,
            short_addr = short_addr,
        )
    }
}

impl fmt::Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RunMode::Init => write!(f, "Initializing..."),
            RunMode::ValueingTicket => write!(f, "Valueing Ticket..."),
            RunMode::PreparingSafe {
                node_address,
                node_xdai,
                node_wxhopr,
                funding_tool,
            } => {
                write!(
                    f,
                    "Waiting for funding on {node_address}({funding_tool}): {node_xdai}, {node_wxhopr}"
                )
            }
            RunMode::Warmup {
                sync_progress,
                hopr_state,
            } => write!(f, "Hopr: {hopr_state}, Syncing... {:.2}%", sync_progress * 100.0),
            RunMode::Running {
                connection,
                funding,
                hopr_state,
            } => write!(f, "Hopr: {hopr_state}, Connection: {connection}, Funding: {funding}"),
        }
    }
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConnectionState::Connecting(dest) => write!(f, "Connecting to {}", dest),
            ConnectionState::Disconnecting(dest) => write!(f, "Disconnecting from {}", dest),
            ConnectionState::Connected(dest) => write!(f, "Connected to {}", dest),
            ConnectionState::Disconnected => write!(f, "Disconnected"),
        }
    }
}

impl fmt::Display for FundingState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FundingState::Unknown => write!(f, "Unknown"),
            FundingState::TopIssue(issue) => write!(f, "Issue: {}", issue),
            FundingState::WellFunded => write!(f, "Well Funded"),
        }
    }
}
