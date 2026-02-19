use edgli::EdgliInitState;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::SystemTime;

use crate::balance::{self, FundingIssue};
use crate::connection;
use crate::connection::destination::{Address, Destination};
use crate::connectivity_health::ConnectivityHealth;
use crate::destination_health::DestinationHealth;
use crate::info::Info;
use crate::log_output;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Command {
    Status,
    Connect(String),
    Metrics,
    Disconnect,
    Balance,
    Ping,
    RefreshNode,
    FundingTool(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(Option<BalanceResponse>),
    Metrics(String),
    Pong,
    Empty,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub run_mode: RunMode,
    pub destinations: Vec<DestinationState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RunMode {
    /// Initial start
    Init,
    /// after creating safe this state will not be reached again
    PreparingSafe {
        node_address: Address,
        node_xdai: Balance<XDai>,
        node_wxhopr: Balance<WxHOPR>,
        funding_tool: balance::FundingTool,
        safe_creation_error: Option<String>,
    },
    /// Safe deployment ongoing
    DeployingSafe { node_address: Address },
    /// Hopr started, determining ticket value for strategies
    Warmup {
        hopr_init_status: Option<HoprInitStatus>,
        hopr_status: Option<HoprStatus>,
    },
    /// Normal operation where connections can be made
    Running {
        funding: FundingState,
        hopr_status: Option<HoprStatus>,
    },
    /// Shutting down service
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum HoprStatus {
    Uninitialized,
    WaitingForFunds,
    CheckingBalance,
    ValidatingNetworkConfig,
    SubscribingToAnnouncements,
    RegisteringSafe,
    AnnouncingNode,
    AwaitingKeyBinding,
    InitializingServices,
    Running,
    Terminated,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum HoprInitStatus {
    ValidatingConfig,
    IdentifyingNode,
    InitializingDatabase,
    ConnectingBlockchain,
    CreatingNode,
    StartingNode,
    Ready,
}

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingState {
    Querying,               // currently checking balances to determine FundingState
    TopIssue(FundingIssue), // there is at least one issue
    WellFunded,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ConnectResponse {
    Connecting(Destination),
    WaitingToConnect(Destination, ConnectivityHealth),
    UnableToConnect(Destination, ConnectivityHealth),
    DestinationNotFound,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    NotConnected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationState {
    pub destination: Destination,
    pub connection_state: ConnectionState,
    pub connectivity: ConnectivityHealth,
    pub exit_health: DestinationHealth,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ConnectionState {
    None,
    Connecting(SystemTime, connection::up::Phase),
    Connected(SystemTime),
    Disconnecting(SystemTime, connection::down::Phase),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BalanceResponse {
    pub node: Balance<XDai>,
    pub safe: Balance<WxHOPR>,
    pub channels_out: Vec<(String, Address, Balance<WxHOPR>)>,
    pub info: Info,
    pub issues: Vec<FundingIssue>,
}

impl RunMode {
    pub fn preparing_safe(
        node_address: Address,
        pre_safe: &Option<balance::PreSafe>,
        funding_tool: balance::FundingTool,
        safe_creation_error: Option<String>,
    ) -> Self {
        RunMode::PreparingSafe {
            node_address,
            node_xdai: pre_safe.clone().map(|s| s.node_xdai).unwrap_or_default(),
            node_wxhopr: pre_safe.clone().map(|s| s.node_wxhopr).unwrap_or_default(),
            funding_tool,
            safe_creation_error,
        }
    }

    pub fn deploying_safe(node_address: Address) -> Self {
        RunMode::DeployingSafe { node_address }
    }

    pub fn warmup(edgli_init_state: Option<EdgliInitState>, hopr_state: Option<HoprState>) -> Self {
        RunMode::Warmup {
            hopr_init_status: edgli_init_state.map(|s| s.into()),
            hopr_status: hopr_state.map(|s| s.into()),
        }
    }

    pub fn running(issues: Option<Vec<FundingIssue>>, hopr_state: Option<HoprState>) -> Self {
        RunMode::Running {
            funding: issues.map(|i| i.into()).unwrap_or(FundingState::Querying),
            hopr_status: hopr_state.map(|s| s.into()),
        }
    }
}

impl ConnectResponse {
    pub fn connecting(destination: Destination) -> Self {
        ConnectResponse::Connecting(destination)
    }
    pub fn waiting(destination: Destination, health: ConnectivityHealth) -> Self {
        ConnectResponse::WaitingToConnect(destination, health)
    }
    pub fn unable(destination: Destination, health: ConnectivityHealth) -> Self {
        ConnectResponse::UnableToConnect(destination, health)
    }
    pub fn destination_not_found() -> Self {
        ConnectResponse::DestinationNotFound
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
        channels_out: HashMap<String, Balance<WxHOPR>>,
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

impl From<HoprState> for HoprStatus {
    fn from(state: HoprState) -> Self {
        match state {
            HoprState::Uninitialized => HoprStatus::Uninitialized,
            HoprState::WaitingForFunds => HoprStatus::WaitingForFunds,
            HoprState::CheckingBalance => HoprStatus::CheckingBalance,
            HoprState::ValidatingNetworkConfig => HoprStatus::ValidatingNetworkConfig,
            HoprState::SubscribingToAnnouncements => HoprStatus::SubscribingToAnnouncements,
            HoprState::RegisteringSafe => HoprStatus::RegisteringSafe,
            HoprState::AnnouncingNode => HoprStatus::AnnouncingNode,
            HoprState::AwaitingKeyBinding => HoprStatus::AwaitingKeyBinding,
            HoprState::InitializingServices => HoprStatus::InitializingServices,
            HoprState::Running => HoprStatus::Running,
            HoprState::Terminated => HoprStatus::Terminated,
        }
    }
}

impl From<EdgliInitState> for HoprInitStatus {
    fn from(state: EdgliInitState) -> Self {
        match state {
            EdgliInitState::ValidatingConfig => HoprInitStatus::ValidatingConfig,
            EdgliInitState::IdentifyingNode => HoprInitStatus::IdentifyingNode,
            EdgliInitState::InitializingDatabase => HoprInitStatus::InitializingDatabase,
            EdgliInitState::ConnectingBlockchain => HoprInitStatus::ConnectingBlockchain,
            EdgliInitState::CreatingNode => HoprInitStatus::CreatingNode,
            EdgliInitState::StartingNode => HoprInitStatus::StartingNode,
            EdgliInitState::Ready => HoprInitStatus::Ready,
        }
    }
}

impl Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RunMode::Init => write!(f, "Initializing"),
            RunMode::PreparingSafe {
                node_address,
                node_xdai,
                node_wxhopr,
                funding_tool,
                safe_creation_error,
            } => {
                if let Some(error) = safe_creation_error {
                    write!(
                        f,
                        "Preparing Safe (node: {}, xdai: {}, wxHOPR: {}, funding tool: {}, error: {})",
                        node_address, node_xdai, node_wxhopr, funding_tool, error
                    )
                } else {
                    write!(
                        f,
                        "Preparing Safe (node: {}, xdai: {}, wxHOPR: {}, funding tool: {})",
                        node_address, node_xdai, node_wxhopr, funding_tool
                    )
                }
            }
            RunMode::DeployingSafe { node_address } => write!(f, "Deploying Safe (node: {})", node_address),
            RunMode::Warmup {
                hopr_init_status,
                hopr_status,
            } => match (hopr_init_status, hopr_status) {
                (None, None) => write!(f, "Warmup"),
                (_, Some(hopr_status)) => write!(f, "Warmup ({hopr_status})"),
                (Some(hopr_init_status), _) => write!(f, "Warmup ({hopr_init_status})"),
            },
            RunMode::Running { funding, hopr_status } => match hopr_status {
                Some(hopr_status) => write!(f, "Ready ({hopr_status}), {funding}"),
                None => write!(f, "Ready, {funding}"),
            },
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
            HoprStatus::Uninitialized => write!(f, "Node is not yet initialized"),
            HoprStatus::WaitingForFunds => write!(f, "Waiting for initial wallet funding"),
            HoprStatus::CheckingBalance => write!(f, "Verifying wallet balance"),
            HoprStatus::ValidatingNetworkConfig => write!(f, "Validating network configuration"),
            HoprStatus::SubscribingToAnnouncements => write!(f, "Subscribing to network announcements"),
            HoprStatus::RegisteringSafe => write!(f, "Registering Safe contract"),
            HoprStatus::AnnouncingNode => write!(f, "Announcing node on chain"),
            HoprStatus::AwaitingKeyBinding => write!(f, "Waiting for on-chain key binding confirmation"),
            HoprStatus::InitializingServices => write!(f, "Initializing internal services"),
            HoprStatus::Running => write!(f, "Node is running"),
            HoprStatus::Terminated => write!(f, "Node has been terminated"),
        }
    }
}

impl Display for HoprInitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HoprInitStatus::ValidatingConfig => write!(f, "Validating configuration"),
            HoprInitStatus::IdentifyingNode => write!(f, "Identifying node"),
            HoprInitStatus::InitializingDatabase => write!(f, "Initializing database"),
            HoprInitStatus::ConnectingBlockchain => write!(f, "Connecting blockchain"),
            HoprInitStatus::CreatingNode => write!(f, "Creating node"),
            HoprInitStatus::StartingNode => write!(f, "Starting node"),
            HoprInitStatus::Ready => write!(f, "Ready"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::RoutingOptions;
    use crate::connectivity_health::{ConnectivityHealth, Health, Need};
    use std::collections::HashMap;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn destination() -> Destination {
        Destination::new(
            "test-destination".to_string(),
            address(1),
            RoutingOptions::IntermediatePath(Default::default()),
            HashMap::new(),
        )
    }

    fn health() -> ConnectivityHealth {
        ConnectivityHealth {
            id: "test-destination".to_string(),
            last_error: None,
            health: Health::ReadyToConnect,
            need: Need::Nothing,
        }
    }

    #[test]
    fn command_should_serialize_and_deserialize_to_the_same_value() -> anyhow::Result<()> {
        let cmd_str = serde_json::to_string(&Command::RefreshNode).expect("serialize refresh command");
        let parsed: Command = cmd_str.parse().expect("parse serialized command");

        assert_eq!(parsed, Command::RefreshNode);
        Ok(())
    }

    #[test]
    fn runmode_running_uses_top_issue_and_hopr_status() -> anyhow::Result<()> {
        let issues = Some(vec![FundingIssue::NodeLowOnFunds]);
        let hopr_state = Some(HoprState::Running);

        match RunMode::running(issues, hopr_state) {
            RunMode::Running { funding, hopr_status } => {
                assert_eq!(funding, FundingState::TopIssue(FundingIssue::NodeLowOnFunds));
                assert_eq!(hopr_status, Some(HoprStatus::Running));
            }
            other => panic!("unexpected run mode {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn funding_state_from_option_applies_priority_rules() -> anyhow::Result<()> {
        let empty: Vec<FundingIssue> = vec![];
        assert_eq!(FundingState::from(empty), FundingState::WellFunded);

        let top = vec![FundingIssue::SafeLowOnFunds];
        assert_eq!(
            FundingState::from(top),
            FundingState::TopIssue(FundingIssue::SafeLowOnFunds)
        );
        Ok(())
    }

    #[test]
    fn connect_response_helpers_cover_all_variants() -> anyhow::Result<()> {
        let dest = destination();
        let resp = ConnectResponse::connecting(dest.clone());
        assert!(matches!(resp, ConnectResponse::Connecting(_)));

        let waiting = ConnectResponse::waiting(dest.clone(), health());
        assert!(matches!(waiting, ConnectResponse::WaitingToConnect(_, _)));

        let unable = ConnectResponse::unable(dest.clone(), health());
        assert!(matches!(unable, ConnectResponse::UnableToConnect(_, _)));

        assert_eq!(
            ConnectResponse::destination_not_found(),
            ConnectResponse::DestinationNotFound
        );
        Ok(())
    }

    #[test]
    fn response_constructors_delegate_to_variant_wrappers() -> anyhow::Result<()> {
        let destination = destination();
        let conn = ConnectResponse::connecting(destination.clone());
        assert!(matches!(Response::connect(conn), Response::Connect(_)));

        let disc = DisconnectResponse::new(destination);
        assert!(matches!(Response::disconnect(disc), Response::Disconnect(_)));

        let status = StatusResponse::new(RunMode::Init, vec![]);
        assert!(matches!(Response::status(status), Response::Status(_)));
        Ok(())
    }
}
