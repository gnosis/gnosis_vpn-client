use edgli::EdgliInitState;
use edgli::hopr_lib::api::node::HoprState;
use edgli::hopr_lib::api::types::primitive::prelude::{Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use crate::balance;
use crate::connection;
use crate::connection::destination::{Address, Destination};
use crate::log_output;
use crate::route_health::{RouteHealth, RouteHealthState};
use crate::serde_utils;

mod balance_response;
pub use balance_response::{BalanceResponse, ChannelBalance, ChannelOut};

/// These commands are sent by the ctl app and forwarded to the core loop for answering
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Command {
    /// Request general status about destinations and connected state
    Status,
    /// Request detailed stats about the current connection, if any
    NerdStats,
    /// Connect to a destination, specified by its id
    Connect(String),
    /// Disconnect from a destination
    Disconnect,
    /// Show channel balance and funding status
    Balance,
    /// Trigger funding tool - only allowed at certain phases
    FundingTool(String),
    /// Return telemetry metrics of the underlying edge client, if running
    Telemetry,
    /// Determine service liveness
    Ping,
    /// Deliver service version and other meta
    Info,
    /// Start worker process and edge client if not already running, with a keep alive duration for the client
    StartClient(Duration),
    /// Stop a running worker process and edge client
    StopClient,
    /// List configured destination IDs
    Destinations,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum WorkerCommand {
    Status,
    NerdStats,
    Connect(String),
    Disconnect,
    Balance,
    FundingTool(String),
    Telemetry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Status(StatusResponse),
    NerdStats(NerdStatsResponse),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(Result<BalanceResponse, String>),
    FundingTool(FundingToolResponse),
    Telemetry(Option<String>),
    Pong,
    Info(InfoResponse),
    StartClient(StartClientResponse),
    StopClient(StopClientResponse),
    Destinations(Vec<String>),
    WorkerOffline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub run_mode: RunMode,
    pub destinations: Vec<DestinationState>,
    pub target_destination: Option<String>,
    pub connecting: Option<ConnectingInfo>,
    pub connected: Option<ConnectedInfo>,
    pub disconnecting: Vec<DisconnectingInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectingInfo {
    pub destination_id: String,
    #[serde(with = "serde_utils::system_time")]
    pub since: SystemTime,
    pub phase: connection::up::Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectedInfo {
    pub destination_id: String,
    #[serde(with = "serde_utils::system_time")]
    pub since: SystemTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisconnectingInfo {
    pub destination_id: String,
    #[serde(with = "serde_utils::system_time")]
    pub since: SystemTime,
    pub phase: connection::down::Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationState {
    pub destination: Destination,
    pub route_health: Option<RouteHealthView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RunMode {
    /// Initial start
    Init { last_error: Option<String> },
    /// after creating safe this state will not be reached again
    PreparingSafe {
        #[serde(with = "serde_utils::address")]
        node_address: Address,
        #[serde(with = "serde_utils::balance")]
        node_xdai: Balance<XDai>,
        #[serde(with = "serde_utils::balance")]
        node_wxhopr: Balance<WxHOPR>,
        funding_tool: Option<String>,
        error: Option<String>,
        balance_recommendation: Option<balance::BalanceRecommendation>,
    },
    /// Safe deployment ongoing
    DeployingSafe {
        #[serde(with = "serde_utils::address")]
        node_address: Address,
    },
    /// Hopr started, determining ticket value for strategies
    Warmup {
        hopr_init_status: Option<HoprInitStatus>,
        hopr_status: Option<HoprStatus>,
    },
    /// Normal operation where connections can be made
    Running { hopr_status: Option<HoprStatus> },
    /// Shutting down edge client,
    Shutdown,
    /// Worker process is not running; only config-level information is available
    NotRunning,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InfoResponse {
    pub version: String,
    pub log_file: Option<PathBuf>,
    pub package_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StartClientResponse {
    Started,
    AlreadyRunning,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StopClientResponse {
    Stopped,
    NotRunning,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum HoprStatus {
    Uninitialized,
    WaitingForFunds,
    CheckingBalance,
    ValidatingNetworkConfig,
    CheckingOnchainAddress,
    RegisteringSafe,
    AnnouncingNode,
    AwaitingKeyBinding,
    InitializingServices,
    Running,
    Terminated,
    Degraded,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum HoprInitStatus {
    ValidatingConfig,
    IdentifyingNode,
    ConnectingBlockchain,
    CreatingNode,
    StartingNode,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConnectResponse {
    AlreadyConnected(Destination),
    Connecting(Destination),
    WaitingToConnect(Destination, RouteHealthState),
    UnableToConnect(Destination, RouteHealthState),
    DestinationNotFound,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DisconnectResponse {
    Disconnecting(Destination),
    NotConnected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FundingToolResponse {
    WrongPhase,
    Started,
    InProgress,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteHealthView {
    pub state: RouteHealthState,
    pub last_error: Option<String>,
    #[serde(with = "serde_utils::opt_system_time")]
    pub checking_since: Option<SystemTime>,
    pub consecutive_failures: u32,
}

impl From<&RouteHealth> for RouteHealthView {
    fn from(rh: &RouteHealth) -> Self {
        RouteHealthView {
            state: rh.state().clone(),
            last_error: rh.last_error().map(str::to_owned),
            checking_since: rh.checking_since(),
            consecutive_failures: rh.consecutive_failures(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NerdStatsResponse {
    NoInfo,
    Connecting(ConnStats),
    Connected(ConnStats),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnStats {
    #[serde(with = "serde_utils::address")]
    pub node_address: Address,
    pub destination: Destination,
    pub wg_pubkey: Option<String>,
    pub wg_server_pubkey: Option<String>,
    pub wg_ip: Option<String>,
    pub session_bound_host: Option<SocketAddr>,
    pub session_id: Option<String>,
}

impl ConnStats {
    pub fn from_conn(conn: &connection::up::Up, node_address: Address) -> Self {
        ConnStats {
            node_address,
            destination: conn.destination.clone(),
            wg_pubkey: conn.wireguard.as_ref().map(|wg| wg.key_pair.public_key.clone()),
            wg_server_pubkey: conn.registration.as_ref().map(|reg| reg.server_public_key()),
            wg_ip: conn.registration.as_ref().map(|reg| reg.address().to_string()),
            session_bound_host: conn.session.as_ref().map(|s| s.bound_host),
            session_id: conn
                .session
                .as_ref()
                .and_then(|s| s.active_clients.first())
                .map(|id| id.to_string()),
        }
    }
}

impl RunMode {
    pub fn preparing_safe(
        node_address: Address,
        pre_safe: &Option<balance::PreSafe>,
        funding_tool: Option<String>,
        error: Option<String>,
        balance_recommendation: Option<balance::BalanceRecommendation>,
    ) -> Self {
        RunMode::PreparingSafe {
            node_address,
            node_xdai: pre_safe.clone().map(|s| s.node_xdai).unwrap_or_default(),
            node_wxhopr: pre_safe.clone().map(|s| s.node_wxhopr).unwrap_or_default(),
            funding_tool,
            error,
            balance_recommendation,
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

    pub fn running(hopr_state: Option<HoprState>) -> Self {
        RunMode::Running {
            hopr_status: hopr_state.map(|s| s.into()),
        }
    }
}

impl ConnectResponse {
    pub fn already_connected(destination: Destination) -> Self {
        ConnectResponse::AlreadyConnected(destination)
    }
    pub fn connecting(destination: Destination) -> Self {
        ConnectResponse::Connecting(destination)
    }
    pub fn waiting(destination: Destination, health: RouteHealthState) -> Self {
        ConnectResponse::WaitingToConnect(destination, health)
    }
    pub fn unable(destination: Destination, health: RouteHealthState) -> Self {
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

impl Response {
    pub fn connect(conn: ConnectResponse) -> Self {
        Response::Connect(conn)
    }

    pub fn disconnect(disc: DisconnectResponse) -> Self {
        Response::Disconnect(disc)
    }

    pub fn nerd_stats(stats: NerdStatsResponse) -> Self {
        Response::NerdStats(stats)
    }

    pub fn status(stat: StatusResponse) -> Self {
        Response::Status(stat)
    }

    pub fn funding_tool(funding_tool: FundingToolResponse) -> Self {
        Response::FundingTool(funding_tool)
    }

    pub fn info(info: InfoResponse) -> Self {
        Response::Info(info)
    }
}

impl Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = log_output::serialize(self);
        write!(f, "{s}")
    }
}

impl Display for WorkerCommand {
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

impl From<HoprState> for HoprStatus {
    fn from(state: HoprState) -> Self {
        match state {
            HoprState::Uninitialized => HoprStatus::Uninitialized,
            HoprState::WaitingForFunds => HoprStatus::WaitingForFunds,
            HoprState::CheckingBalance => HoprStatus::CheckingBalance,
            HoprState::ValidatingNetworkConfig => HoprStatus::ValidatingNetworkConfig,
            HoprState::CheckingOnchainAddress => HoprStatus::CheckingOnchainAddress,
            HoprState::RegisteringSafe => HoprStatus::RegisteringSafe,
            HoprState::AnnouncingNode => HoprStatus::AnnouncingNode,
            HoprState::AwaitingKeyBinding => HoprStatus::AwaitingKeyBinding,
            HoprState::InitializingServices => HoprStatus::InitializingServices,
            HoprState::Running => HoprStatus::Running,
            HoprState::Terminated => HoprStatus::Terminated,
            HoprState::Degraded => HoprStatus::Degraded,
            HoprState::Failed => HoprStatus::Failed,
        }
    }
}

impl From<EdgliInitState> for HoprInitStatus {
    fn from(state: EdgliInitState) -> Self {
        match state {
            EdgliInitState::ValidatingConfig => HoprInitStatus::ValidatingConfig,
            EdgliInitState::IdentifyingNode => HoprInitStatus::IdentifyingNode,
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
            RunMode::Init { last_error: None } => write!(f, "Initializing"),
            RunMode::Init { last_error: Some(err) } => write!(f, "Initializing (last error: {err})"),
            RunMode::PreparingSafe {
                node_address,
                node_xdai,
                node_wxhopr,
                funding_tool,
                error,
                balance_recommendation,
            } => {
                let mut msg = format!(
                    "Preparing Safe (node: {}, xdai: {node_xdai}, wxHOPR: {node_wxhopr}",
                    node_address.to_checksum()
                );
                if let Some(rec) = balance_recommendation {
                    msg = format!("{msg}, recommended: wxHOPR >= {}, xDAI >= {}", rec.wxhopr, rec.xdai);
                }
                msg = match (funding_tool, error) {
                    (Some(tool), Some(error)) => {
                        format!("{msg}, funding tool: {tool}, error: {error})")
                    }
                    (Some(tool), None) => format!("{msg}, funding tool: {tool})"),
                    (None, Some(error)) => format!("{msg}, error: {error})"),
                    (None, None) => format!("{msg})"),
                };
                write!(f, "{}", msg)
            }
            RunMode::DeployingSafe { node_address } => {
                write!(f, "Deploying Safe (node: {})", node_address.to_checksum())
            }
            RunMode::Warmup {
                hopr_init_status,
                hopr_status,
            } => match (hopr_init_status, hopr_status) {
                (None, None) => write!(f, "Warmup"),
                (_, Some(hopr_status)) => write!(f, "Warmup ({hopr_status})"),
                (Some(hopr_init_status), _) => write!(f, "Warmup ({hopr_init_status})"),
            },
            RunMode::Running { hopr_status } => match hopr_status {
                Some(s) => write!(f, "Ready ({s})"),
                None => write!(f, "Ready"),
            },
            RunMode::Shutdown => write!(f, "Shutting down"),
            RunMode::NotRunning => write!(f, "Worker offline"),
        }
    }
}

impl Display for ConnectingInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Connecting to {} (since {}, phase {})",
            self.destination_id,
            log_output::elapsed(&self.since),
            self.phase
        )
    }
}

impl Display for ConnectedInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Connected to {} (since {})",
            self.destination_id,
            log_output::elapsed(&self.since)
        )
    }
}

impl Display for DisconnectingInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Disconnecting from {} (since {}, phase {})",
            self.destination_id,
            log_output::elapsed(&self.since),
            self.phase
        )
    }
}

impl Display for HoprStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HoprStatus::Uninitialized => write!(f, "Node is not yet initialized"),
            HoprStatus::WaitingForFunds => write!(f, "Waiting for initial wallet funding"),
            HoprStatus::CheckingBalance => write!(f, "Verifying wallet balance"),
            HoprStatus::ValidatingNetworkConfig => write!(f, "Validating network configuration"),
            HoprStatus::CheckingOnchainAddress => write!(f, "Checking onchain address"),
            HoprStatus::RegisteringSafe => write!(f, "Registering Safe contract"),
            HoprStatus::AnnouncingNode => write!(f, "Announcing node on chain"),
            HoprStatus::AwaitingKeyBinding => write!(f, "Waiting for on-chain key binding confirmation"),
            HoprStatus::InitializingServices => write!(f, "Initializing internal services"),
            HoprStatus::Running => write!(f, "Node is running"),
            HoprStatus::Terminated => write!(f, "Node has been terminated"),
            HoprStatus::Degraded => write!(f, "Node is running in degraded state"),
            HoprStatus::Failed => write!(f, "Node has failed"),
        }
    }
}

impl Display for HoprInitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HoprInitStatus::ValidatingConfig => write!(f, "Validating configuration"),
            HoprInitStatus::IdentifyingNode => write!(f, "Identifying node"),
            HoprInitStatus::ConnectingBlockchain => write!(f, "Connecting blockchain"),
            HoprInitStatus::CreatingNode => write!(f, "Creating node"),
            HoprInitStatus::StartingNode => write!(f, "Starting node"),
            HoprInitStatus::Ready => write!(f, "Ready"),
        }
    }
}

impl TryFrom<Command> for WorkerCommand {
    type Error = ();

    fn try_from(value: Command) -> Result<Self, Self::Error> {
        match value {
            Command::Status => Ok(WorkerCommand::Status),
            Command::NerdStats => Ok(WorkerCommand::NerdStats),
            Command::Connect(dest) => Ok(WorkerCommand::Connect(dest)),
            Command::Disconnect => Ok(WorkerCommand::Disconnect),
            Command::Balance => Ok(WorkerCommand::Balance),
            Command::FundingTool(secret) => Ok(WorkerCommand::FundingTool(secret)),
            Command::Telemetry => Ok(WorkerCommand::Telemetry),
            // Commands that are not relevant for the worker
            Command::Info | Command::Ping | Command::StartClient(_) | Command::StopClient | Command::Destinations => {
                Err(())
            }
        }
    }
}

impl Display for RouteHealthView {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.state)?;
        if let Some(since) = &self.checking_since {
            write!(f, " (checking since {})", crate::log_output::elapsed(since))?;
        }
        if self.consecutive_failures > 0 {
            write!(f, " ({} consecutive failures)", self.consecutive_failures)?;
        }
        if let Some(err) = &self.last_error {
            write!(f, " (last error: {err})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::HopRouting;
    use crate::gvpn_client;
    use crate::route_health::ExitHealth;
    use std::collections::HashMap;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn destination() -> Destination {
        Destination::new(
            "test-destination".to_string(),
            address(1),
            HopRouting::try_from(1).expect("conversion cannot fail"),
            HashMap::new(),
        )
    }

    fn route_health_state() -> RouteHealthState {
        RouteHealthState::ReadyToConnect {
            exit: ExitHealth {
                checked_at: SystemTime::now(),
                versions: gvpn_client::Versions {
                    versions: vec!["v1".to_string()],
                    latest: "v1".to_string(),
                },
                ping_rtt: Duration::from_millis(100),
                health: gvpn_client::Health {
                    slots: gvpn_client::Slots {
                        available: 10,
                        connected: 1,
                    },
                    load_avg: gvpn_client::LoadAvg {
                        one: 0.1,
                        five: 0.2,
                        fifteen: 0.3,
                        nproc: 4,
                    },
                },
            },
        }
    }

    #[test]
    fn command_should_serialize_and_deserialize_to_the_same_value() -> anyhow::Result<()> {
        let cmd_str = serde_json::to_string(&Command::Balance).expect("serialize balance command");
        let parsed: Command = cmd_str.parse().expect("parse serialized command");

        assert_eq!(parsed, Command::Balance);
        Ok(())
    }

    #[test]
    fn runmode_running_passes_through_hopr_status() -> anyhow::Result<()> {
        let hopr_state = Some(HoprState::Running);

        match RunMode::running(hopr_state) {
            RunMode::Running { hopr_status } => {
                assert_eq!(hopr_status, Some(HoprStatus::Running));
            }
            other => panic!("unexpected run mode {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn connect_response_helpers_cover_all_variants() -> anyhow::Result<()> {
        let dest = destination();
        let resp = ConnectResponse::connecting(dest.clone());
        assert!(matches!(resp, ConnectResponse::Connecting(_)));

        let waiting = ConnectResponse::waiting(dest.clone(), route_health_state());
        assert!(matches!(waiting, ConnectResponse::WaitingToConnect(_, _)));

        let unable = ConnectResponse::unable(dest.clone(), route_health_state());
        assert!(matches!(unable, ConnectResponse::UnableToConnect(_, _)));

        assert!(matches!(
            ConnectResponse::destination_not_found(),
            ConnectResponse::DestinationNotFound
        ));
        Ok(())
    }

    #[test]
    fn response_constructors_delegate_to_variant_wrappers() -> anyhow::Result<()> {
        let destination = destination();
        let conn = ConnectResponse::connecting(destination.clone());
        assert!(matches!(Response::connect(conn), Response::Connect(_)));

        let disc = DisconnectResponse::new(destination);
        assert!(matches!(Response::disconnect(disc), Response::Disconnect(_)));

        Ok(())
    }

    #[test]
    fn runmode_init_serializes_to_expected_json_shape() {
        // Asserting the exact string rather than a serde_json::Value is intentional:
        // serde_json serializes struct fields in definition order (not a HashMap), so the
        // output is deterministic. The string form documents the exact wire contract and
        // will catch serde attribute changes (e.g. rename, rename_all) just as well as a
        // Value comparison would, while keeping the expected payload directly readable.
        let no_error = serde_json::to_string(&RunMode::Init { last_error: None }).unwrap();
        assert_eq!(no_error, r#"{"Init":{"last_error":null}}"#);

        let with_error = serde_json::to_string(&RunMode::Init {
            last_error: Some("connection refused".into()),
        })
        .unwrap();
        assert_eq!(with_error, r#"{"Init":{"last_error":"connection refused"}}"#);
    }

    #[test]
    fn runmode_init_deserializes_from_json_fixture() {
        let no_error: RunMode = serde_json::from_str(r#"{"Init":{"last_error":null}}"#).unwrap();
        assert!(matches!(no_error, RunMode::Init { last_error: None }));

        let with_error: RunMode = serde_json::from_str(r#"{"Init":{"last_error":"connection refused"}}"#).unwrap();
        assert!(matches!(with_error, RunMode::Init { last_error: Some(ref e) } if e == "connection refused"));
    }
}
