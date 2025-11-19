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

#[derive(Debug, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum HoprStatus {
    Running,
    Syncing,
    Starting,
    Indexing,
    Initializing,
    Uninitialized,
}

// in order of priority
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingState {
    Querying,               // currently checking balances to determine FundingState
    TopIssue(FundingIssue), // there is at least one issue
    WellFunded,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ConnectResponse {
    Connecting(Destination),
    WaitingToConnect(Destination, Option<DestinationHealth>),
    UnableToConnect(Destination, DestinationHealth),
    AddressNotFound,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    NotConnected,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct DestinationState {
    pub destination: Destination,
    pub connection_state: ConnectionState,
    pub health: Option<DestinationHealth>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ConnectionState {
    None,
    Connecting(SystemTime, connection::up::Phase),
    Connected(SystemTime),
    Disconnecting(SystemTime, connection::down::Phase),
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
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
        pre_safe: &Option<balance::PreSafe>,
        funding_tool: balance::FundingTool,
    ) -> Self {
        RunMode::PreparingSafe {
            node_address,
            node_xdai: pre_safe.clone().map(|s| s.node_xdai).unwrap_or_default(),
            node_wxhopr: pre_safe.clone().map(|s| s.node_wxhopr).unwrap_or_default(),
            funding_tool,
        }
    }

    pub fn warmup(hopr_state: &Option<HoprState>) -> Self {
        RunMode::Warmup {
            hopr_status: hopr_state.into(),
        }
    }

    pub fn running(issues: &Option<Vec<FundingIssue>>, hopr_state: &Option<HoprState>) -> Self {
        RunMode::Running {
            funding: issues.into(),
            hopr_status: hopr_state.into(),
        }
    }
}

impl ConnectResponse {
    pub fn connecting(destination: Destination) -> Self {
        ConnectResponse::Connecting(destination)
    }
    pub fn waiting(destination: Destination, health: Option<DestinationHealth>) -> Self {
        ConnectResponse::WaitingToConnect(destination, health)
    }
    pub fn unable(destination: Destination, health: DestinationHealth) -> Self {
        ConnectResponse::UnableToConnect(destination, health)
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

impl From<&Option<Vec<FundingIssue>>> for FundingState {
    fn from(issues: &Option<Vec<FundingIssue>>) -> Self {
        match issues {
            Some(issues) => {
                if issues.is_empty() {
                    FundingState::WellFunded
                } else {
                    FundingState::TopIssue(issues[0].clone())
                }
            }
            None => FundingState::Querying,
        }
    }
}

impl From<&Option<HoprState>> for HoprStatus {
    fn from(state: &Option<HoprState>) -> Self {
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
            FundingState::Querying => write!(f, "Determining funding"),
            FundingState::TopIssue(issue) => write!(f, "Issue: {}", issue),
            FundingState::WellFunded => write!(f, "Well funded"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::RoutingOptions;
    use crate::connection::destination_health::{DestinationHealth, Health, Need};
    use std::collections::HashMap;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn destination() -> Destination {
        Destination::new(
            address(1),
            RoutingOptions::IntermediatePath(Default::default()),
            HashMap::new(),
        )
    }

    fn health() -> DestinationHealth {
        DestinationHealth {
            last_error: None,
            health: Health::ReadyToConnect,
            need: Need::Nothing,
        }
    }

    #[test]
    fn command_from_str_round_trip() {
        let cmd_str = serde_json::to_string(&Command::RefreshNode).expect("json");
        let parsed: Command = cmd_str.parse().expect("parse");

        assert_eq!(parsed, Command::RefreshNode);
    }

    #[test]
    fn runmode_running_reflects_inputs() {
        let issues = Some(vec![FundingIssue::NodeLowOnFunds]);
        let hopr_state = Some(HoprState::Running);

        match RunMode::running(&issues, &hopr_state) {
            RunMode::Running { funding, hopr_status } => {
                assert_eq!(funding, FundingState::TopIssue(FundingIssue::NodeLowOnFunds));
                assert_eq!(hopr_status, HoprStatus::Running);
            }
            other => panic!("unexpected run mode {other:?}"),
        }
    }

    #[test]
    fn funding_state_from_options() {
        assert_eq!(FundingState::from(&None), FundingState::Querying);

        let empty: Option<Vec<FundingIssue>> = Some(vec![]);
        assert_eq!(FundingState::from(&empty), FundingState::WellFunded);

        let top = Some(vec![FundingIssue::SafeLowOnFunds]);
        assert_eq!(
            FundingState::from(&top),
            FundingState::TopIssue(FundingIssue::SafeLowOnFunds)
        );
    }

    #[test]
    fn connect_response_helpers_build_variants() {
        let dest = destination();
        let resp = ConnectResponse::connecting(dest.clone());
        assert_eq!(matches!(resp, ConnectResponse::Connecting(_)), true);

        let waiting = ConnectResponse::waiting(dest.clone(), Some(health()));
        assert!(matches!(waiting, ConnectResponse::WaitingToConnect(_, Some(_))));

        let unable = ConnectResponse::unable(dest.clone(), health());
        assert!(matches!(unable, ConnectResponse::UnableToConnect(_, _)));

        assert_eq!(ConnectResponse::address_not_found(), ConnectResponse::AddressNotFound);
    }

    #[test]
    fn response_constructors_wrap_variants() {
        let destination = destination();
        let conn = ConnectResponse::connecting(destination.clone());
        assert!(matches!(Response::connect(conn), Response::Connect(_)));

        let disc = DisconnectResponse::new(destination);
        assert!(matches!(Response::disconnect(disc), Response::Disconnect(_)));

        let status = StatusResponse::new(RunMode::Init, vec![]);
        assert!(matches!(Response::status(status), Response::Status(_)));
    }
}
