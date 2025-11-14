use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::SystemTime;

use crate::balance::{self, FundingIssue};
use crate::connection;
use crate::connection::destination::{Address, Destination};
use crate::connection::destination_health::DestinationHealth;
use crate::info::Info;
use crate::log_output;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(Address),
    Metrics,
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
    Metrics(String),
    Pong,
    Empty,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub run_mode: RunMode,
    pub destinations: Vec<DestinationState>,
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
    Warmup { hopr_status: HoprStatus },
    /// Normal operation where connections can be made
    Running {
        funding: FundingState,
        hopr_status: HoprStatus,
    },
    /// Shutting down service
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HoprStatus {
    Running,
    Syncing,
    Starting,
    Indexing,
    Initializing,
    Uninitialized,
}

// in order of priority
#[derive(Debug, Default, Serialize, Deserialize)]
pub enum FundingState {
    #[default]
    Unknown, // state not queried yet
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
pub struct DestinationState {
    pub destination: Destination,
    pub connection_state: ConnectionState,
    pub health: Option<DestinationHealth>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ConnectionState {
    None,
    Connecting(SystemTime, connection::up::Phase),
    Connected(SystemTime),
    Disconnecting(SystemTime, connection::down::Phase),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub node: Balance<XDai>,
    pub safe: Balance<WxHOPR>,
    pub channels_out: Balance<WxHOPR>,
    pub info: Info,
    pub issues: Vec<FundingIssue>,
}

impl RunMode {
    pub fn preparing_safe(
        node_address: Address,
        pre_safe: Option<balance::PreSafe>,
        funding_tool: balance::FundingTool,
    ) -> Self {
        RunMode::PreparingSafe {
            node_address,
            node_xdai: pre_safe.clone().map(|s| s.node_xdai).unwrap_or_default(),
            node_wxhopr: pre_safe.map(|s| s.node_wxhopr).unwrap_or_default(),
            funding_tool,
        }
    }

    pub fn warmup(hopr_state: Option<HoprState>) -> Self {
        RunMode::Warmup {
            hopr_status: hopr_state.into(),
        }
    }

    pub fn running(funding: FundingState, hopr_state: Option<HoprState>) -> Self {
        RunMode::Running {
            funding,
            hopr_status: hopr_state.into(),
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
    pub fn new(run_mode: RunMode, destinations: Vec<DestinationState>) -> Self {
        StatusResponse { run_mode, destinations }
    }
}

impl BalanceResponse {
    pub fn new(
        node: Balance<XDai>,
        safe: Balance<WxHOPR>,
        channels_out: Balance<WxHOPR>,
        issues: Vec<FundingIssue>,
        info: Info,
    ) -> Self {
        BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
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

impl Display for Command {
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

impl From<Vec<FundingIssue>> for FundingState {
    fn from(issues: Vec<FundingIssue>) -> Self {
        if issues.is_empty() {
            FundingState::WellFunded
        } else {
            FundingState::TopIssue(issues[0].clone())
        }
    }
}

impl From<Option<HoprState>> for HoprStatus {
    fn from(state: Option<HoprState>) -> Self {
        match state {
            Some(HoprState::Running) => HoprStatus::Running,
            Some(HoprState::Starting) => HoprStatus::Starting,
            Some(HoprState::Indexing) => HoprStatus::Indexing,
            Some(HoprState::Initializing) => HoprStatus::Initializing,
            Some(HoprState::Uninitialized) => HoprStatus::Uninitialized,
            None => HoprStatus::Uninitialized,
        }
    }
}

impl Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RunMode::Init => write!(f, "Initializing"),
            RunMode::ValueingTicket => write!(f, "Determine ticket value"),
            RunMode::PreparingSafe {
                node_address,
                node_xdai,
                node_wxhopr,
                funding_tool,
            } => {
                write!(
                    f,
                    "Waiting for funding on {node_address}({node_xdai}, {node_wxhopr}) - {funding_tool}"
                )
            }
            RunMode::Warmup { hopr_status } => {
                write!(f, "Warmup (Hopr {})", hopr_status)
            }
            RunMode::Running { funding, hopr_status } => {
                write!(f, "Ready (Hopr {hopr_status}), {funding}")
            }
            RunMode::Shutdown => write!(f, "Shutting down"),
        }
    }
}

impl Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConnectionState::None => write!(f, "Not Connected"),
            ConnectionState::Connecting(since, phase) => {
                write!(f, "Connecting (since {}): {:?}", log_output::elapsed(since), phase)
            }
            ConnectionState::Connected(since) => {
                write!(f, "Connected (since {})", log_output::elapsed(since))
            }
            ConnectionState::Disconnecting(since, phase) => {
                write!(f, "Disconnecting (since {}): {:?}", log_output::elapsed(since), phase)
            }
        }
    }
}

impl Display for DestinationState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let output = format!("{} - {}", self.destination, self.connection_state);
        if let Some(health) = self.health.as_ref() {
            write!(f, "{} (Health: {})", output, health)
        } else {
            write!(f, "{}", output)
        }
    }
}

impl Display for FundingState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FundingState::Unknown => write!(f, "Unknown"),
            FundingState::TopIssue(issue) => write!(f, "Issue: {}", issue),
            FundingState::WellFunded => write!(f, "Well Funded"),
        }
    }
}

impl Display for HoprStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HoprStatus::Running => write!(f, "Running"),
            HoprStatus::Syncing => write!(f, "Syncing"),
            HoprStatus::Starting => write!(f, "Starting"),
            HoprStatus::Indexing => write!(f, "Indexing"),
            HoprStatus::Initializing => write!(f, "Initializing"),
            HoprStatus::Uninitialized => write!(f, "Uninitialized"),
        }
    }
}
