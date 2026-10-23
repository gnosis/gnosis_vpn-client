//! Every socket message; enums are `{"type": ...}` objects, `Response` adds `data` as its payloads are tagged too.
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::values::{
    Address, ApiVersion, Balance, CountryCode, DestinationId, FundingSecret, Hops, PeerId, SessionId, WgPublicKey,
    WxHopr, XDai, duration_ms, unix_ms,
};

/// Bumped on every incompatible wire change; the service refuses requests of any other version.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: u32,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Status,
    NerdStats,
    Connect {
        destination: DestinationId,
    },
    Disconnect,
    Balance,
    FundingTool {
        secret: FundingSecret,
    },
    Telemetry,
    Ping,
    Info,
    StartClient {
        #[serde(with = "duration_ms")]
        timeout: Duration,
    },
    StopClient,
    Destinations,
    Probe {
        destination: DestinationId,
    },
    Unprobe,
    QuickProbe {
        destination: DestinationId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ok { response: Response },
    Err { error: ServiceError },
}

/// Failures any request can hit, before or instead of its own response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceError {
    #[error("the service speaks protocol version {service}, this client {client}", client = PROTOCOL_VERSION)]
    ProtocolMismatch { service: u32 },
    #[error("the service could not read the request: {reason}")]
    InvalidRequest { reason: String },
    #[error("worker client is currently offline - use command `start-client` to start it")]
    WorkerOffline,
    #[error("worker client is restarting - try again shortly")]
    WorkerRestarting,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Response {
    Status(Box<Status>),
    NerdStats(Box<NerdStats>),
    Connect(ConnectResponse),
    Disconnect(DisconnectResponse),
    Balance(BalanceResponse),
    FundingTool(FundingToolResponse),
    /// Prometheus text; none while the edge client is not running.
    Telemetry(Option<String>),
    Pong,
    Info(InfoResponse),
    StartClient(StartClientResponse),
    StopClient(StopClientResponse),
    Destinations(Vec<DestinationId>),
    Probe(ProbeResponse),
    Unprobe(UnprobeResponse),
    QuickProbe(QuickProbeResponse),
}

// ---- status ----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub run_mode: RunMode,
    pub destinations: Vec<DestinationStatus>,
    /// Where the service is headed; set before and during every connection attempt.
    pub target: Option<DestinationId>,
    pub connection: Connection,
    /// Teardowns run in the background and may overlap a new connection.
    pub disconnecting: Vec<Disconnecting>,
    pub probe: Option<ProbeView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Connection {
    Disconnected,
    Connecting {
        destination: DestinationId,
        #[serde(with = "unix_ms")]
        since: SystemTime,
        phase: ConnectPhase,
    },
    /// Between attempts a reconnect has no phase: it waits for route health.
    Reconnecting {
        destination: DestinationId,
        #[serde(with = "unix_ms")]
        since: SystemTime,
        phase: Option<ConnectPhase>,
    },
    Connected {
        destination: DestinationId,
        #[serde(with = "unix_ms")]
        since: SystemTime,
        #[serde(with = "duration_ms::option")]
        tunnel_ping_rtt: Option<Duration>,
        stall: Option<TunnelStall>,
    },
}

impl Connection {
    pub fn destination(&self) -> Option<&DestinationId> {
        match self {
            Connection::Disconnected => None,
            Connection::Connecting { destination, .. }
            | Connection::Reconnecting { destination, .. }
            | Connection::Connected { destination, .. } => Some(destination),
        }
    }
}

/// Consecutive tunnel pings failing; the service reconnects once `failed_pings` reaches `reconnect_at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TunnelStall {
    #[serde(with = "unix_ms")]
    pub since: SystemTime,
    pub failed_pings: u32,
    pub reconnect_at: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disconnecting {
    pub destination: DestinationId,
    #[serde(with = "unix_ms")]
    pub since: SystemTime,
    pub phase: DisconnectPhase,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectPhase {
    Init,
    GatherPeerIps,
    KillswitchLockdown,
    GeneratingWg,
    RegisterWg,
    OpeningPing,
    EstablishWgTunnel,
    VerifyPing,
    AdjustToMain,
    ConnectionEstablished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisconnectPhase {
    Disconnecting,
    OpeningBridge,
    UnregisterWg,
    ClosingBridge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunMode {
    Init {
        last_error: Option<String>,
    },
    PreparingSafe {
        node_address: Address,
        node_xdai: Balance<XDai>,
        node_wxhopr: Balance<WxHopr>,
        funding_tool: Option<String>,
        error: Option<String>,
        balance_recommendation: Option<Box<BalanceRecommendation>>,
    },
    DeployingSafe {
        node_address: Address,
    },
    Warmup {
        stage: WarmupStage,
        last_error: Option<String>,
    },
    Running {
        hopr_status: Option<HoprStatus>,
        funding_status: Option<FundingStatus>,
        channel_maintenance: ChannelMaintenance,
    },
    Shutdown,
    Restarting,
    NotRunning,
}

/// Edge-client start-up followed by node start-up, as one progression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WarmupStage {
    Initializing,
    ValidatingConfig,
    IdentifyingNode,
    ConnectingBlockchain,
    CreatingNode,
    StartingNode,
    Ready,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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

impl From<HoprStatus> for WarmupStage {
    fn from(status: HoprStatus) -> Self {
        match status {
            HoprStatus::Uninitialized => WarmupStage::Uninitialized,
            HoprStatus::WaitingForFunds => WarmupStage::WaitingForFunds,
            HoprStatus::CheckingBalance => WarmupStage::CheckingBalance,
            HoprStatus::ValidatingNetworkConfig => WarmupStage::ValidatingNetworkConfig,
            HoprStatus::CheckingOnchainAddress => WarmupStage::CheckingOnchainAddress,
            HoprStatus::RegisteringSafe => WarmupStage::RegisteringSafe,
            HoprStatus::AnnouncingNode => WarmupStage::AnnouncingNode,
            HoprStatus::AwaitingKeyBinding => WarmupStage::AwaitingKeyBinding,
            HoprStatus::InitializingServices => WarmupStage::InitializingServices,
            HoprStatus::Running => WarmupStage::Running,
            HoprStatus::Terminated => WarmupStage::Terminated,
            HoprStatus::Degraded => WarmupStage::Degraded,
            HoprStatus::Failed => WarmupStage::Failed,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelMaintenance {
    #[default]
    Ok,
    Unavailable {
        #[serde(with = "unix_ms")]
        since: SystemTime,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationStatus {
    pub destination: Destination,
    pub route_health: Option<RouteHealthView>,
}

// ---- destinations ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: DestinationId,
    pub meta: Meta,
    /// Derived from the exit's announced address; none until a lookup resolved it.
    pub geo: Option<Geo>,
    pub address: Address,
    pub hops: Hops,
    pub gnosis_vpn_server: SocketAddr,
    pub wireguard_server: SocketAddr,
    pub source: DestinationSource,
    pub overrides: Overrides,
}

/// Operator-published labels: the keys we understand, plus everything else as published.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub name: Option<String>,
    pub description: Option<String>,
    pub other: BTreeMap<String, String>,
}

impl Meta {
    /// The only place the recognized label names are known.
    pub fn from_labels(labels: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut other: BTreeMap<String, String> = labels.into_iter().collect();
        Self {
            name: other.remove("name"),
            description: other.remove("description"),
            other,
        }
    }
}

/// What configuration pinned; every key present is a pin a discovery tick may not overwrite.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overrides {
    pub configured_meta: BTreeMap<String, String>,
    pub configured_gnosis_vpn_server: Option<SocketAddr>,
    pub configured_wireguard_server: Option<SocketAddr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DestinationSource {
    Configured,
    Discovered,
    ConfiguredAndDiscovered,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geo {
    pub country: CountryCode,
    pub source: GeoSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeoSource {
    AnnouncedIp,
}

/// The suffix a discovered id carries so no later same-named exit can take it; FNV-1a by hand, as `DefaultHasher` drifts per rustc.
pub fn exit_discriminator(address: &Address, hops: Hops) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut hash = OFFSET;
    for byte in format!("{address}@{hops}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (0..4)
        .map(|_| {
            let c = ALPHABET[(hash % ALPHABET.len() as u64) as usize] as char;
            hash /= ALPHABET.len() as u64;
            c
        })
        .collect()
}

// ---- route health ----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteHealthView {
    pub state: RouteHealthState,
    pub last_error: Option<String>,
    pub walk: Option<RouteWalk>,
    pub quick_probe: Option<QuickProbeState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteHealthState {
    Unrecoverable {
        reason: UnrecoverableReason,
    },
    /// The graph holds no plannable path to the exit right now.
    NotRoutable,
    Routable,
}

/// `NotAllowed` and `CannotOpenSession` need a config change; `IncompatibleApiVersion` an exit upgrade.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnrecoverableReason {
    /// 0-hop without `--allow-insecure`, or 2+ hops without `--allow-experimental`.
    NotAllowed,
    IncompatibleApiVersion {
        offered: Vec<ApiVersion>,
    },
    CannotOpenSession {
        error: String,
    },
}

/// The last graph walk for an exit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteWalk {
    /// The exit's chain address maps to no packet key - it never announced itself.
    NotAnnounced {
        #[serde(with = "unix_ms")]
        walked_at: SystemTime,
    },
    /// No path survived selection and chain validation in at least one direction.
    NoPath {
        #[serde(with = "unix_ms")]
        walked_at: SystemTime,
        forward_count: u32,
        return_count: u32,
    },
    Paths {
        #[serde(with = "unix_ms")]
        walked_at: SystemTime,
        count: u32,
        /// 1 means a single point of failure; 0 only on a direct route.
        distinct_first_relays: u32,
        /// Relays of the best-valued path, in path order.
        best_relays: Vec<Address>,
        /// In (0.0, 1.0]; higher is better.
        best_value: f64,
        return_count: u32,
        return_best_relays: Vec<Address>,
        return_best_value: f64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuickProbeState {
    Checking {
        #[serde(with = "unix_ms")]
        since: SystemTime,
        /// The result this check refreshes, so a re-check does not blank the exit's data.
        last: Option<QuickProbeCheck>,
    },
    Checked(QuickProbeCheck),
    Failed {
        #[serde(with = "unix_ms")]
        checked_at: SystemTime,
        error: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuickProbeCheck {
    #[serde(with = "unix_ms")]
    pub checked_at: SystemTime,
    pub versions: ExitVersions,
    /// The version this client selected from `versions`; none means incompatible.
    pub api_version: Option<ApiVersion>,
    pub load: Health,
    /// Includes the exit's status-generation time.
    #[serde(with = "duration_ms")]
    pub status_rtt: Duration,
}

/// What an exit server publishes about itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExitVersions {
    pub api: Vec<ApiVersion>,
    /// Absent on exits that predate publishing it.
    pub server: Option<semver::Version>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub slots: Slots,
    pub load_avg: LoadAvg,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Slots {
    pub total: u32,
    pub available: u32,
    pub connected: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadAvg {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
    pub nproc: u16,
}

// ---- probe ----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeView {
    pub destination: DestinationId,
    pub state: ProbeState,
    #[serde(with = "unix_ms::option")]
    pub session_since: Option<SystemTime>,
    pub versions: Option<ExitVersions>,
    pub api_version: Option<ApiVersion>,
    #[serde(with = "duration_ms::option")]
    pub ping_rtt: Option<Duration>,
    pub load: Option<Health>,
    #[serde(with = "unix_ms::option")]
    pub checked_at: Option<SystemTime>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeState {
    /// No session yet, or the last open attempt failed and a retry is pending.
    Opening,
    /// Session open, waiting for the first result of every check.
    Checking,
    /// Every check has succeeded at least once on this session.
    Ready,
    /// Too many consecutive failures; a new session is being opened.
    Reopening,
}

// ---- command responses ----

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectResponse {
    AlreadyConnected {
        destination: Box<Destination>,
    },
    Connecting {
        destination: Box<Destination>,
    },
    WaitingToConnect {
        destination: Box<Destination>,
        route_health: RouteHealthState,
    },
    UnableToConnect {
        destination: Box<Destination>,
        route_health: RouteHealthState,
    },
    DestinationNotFound,
    /// One exit reached by several paths; the caller must pick one.
    DestinationAmbiguous {
        candidates: Vec<DestinationId>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisconnectResponse {
    Disconnecting { destination: Box<Destination> },
    NotConnected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeResponse {
    Probing {
        destination: Box<Destination>,
    },
    Replaced {
        destination: Box<Destination>,
        previous: Box<Destination>,
    },
    AlreadyProbing {
        destination: Box<Destination>,
    },
    InUse {
        destination: Box<Destination>,
    },
    UnableToProbe {
        destination: Box<Destination>,
        route_health: RouteHealthState,
    },
    NotReady,
    DestinationNotFound,
    DestinationAmbiguous {
        candidates: Vec<DestinationId>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnprobeResponse {
    Closing { destination: Box<Destination> },
    InUse { destination: Box<Destination> },
    NotProbing,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuickProbeResponse {
    Checking {
        destination: Box<Destination>,
    },
    UnableToProbe {
        destination: Box<Destination>,
        route_health: RouteHealthState,
    },
    AlreadyProbing {
        destination: Box<Destination>,
    },
    AlreadyChecking {
        destination: Box<Destination>,
    },
    NotReady,
    DestinationNotFound,
    DestinationAmbiguous {
        candidates: Vec<DestinationId>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FundingToolResponse {
    WrongPhase,
    Started,
    InProgress,
    Done,
    /// A successful run is only restartable after a cooldown.
    Cooldown {
        #[serde(with = "duration_ms")]
        remaining: Duration,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InfoResponse {
    pub version: semver::Version,
    /// The installer's version, a different numbering than the service's.
    pub package_version: Option<String>,
    pub log_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StartClientResponse {
    Started,
    AlreadyRunning,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopClientResponse {
    Stopped,
    NotRunning,
}

// ---- balance ----

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BalanceResponse {
    Available(Box<Balances>),
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balances {
    pub node: Balance<XDai>,
    pub safe: Balance<WxHopr>,
    pub channels_out: Vec<ChannelOut>,
    pub info: NodeInfo,
    pub capacity_allocations: Option<CapacityAllocations>,
    pub ideal_balance: Option<BalanceRecommendation>,
    pub funding_status: Option<FundingStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelOut {
    pub address: Address,
    /// None while the channel's balance is still being queried.
    pub balance: Option<Balance<WxHopr>>,
    /// The first destination id, in sort order, whose exit owns this channel.
    pub matched_exit: Option<DestinationId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_address: Address,
    pub node_peer_id: PeerId,
    pub safe_address: Address,
}

/// Data-throughput capacity of a wxHOPR stake at the current ticket price.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capacity {
    pub stake: Balance<WxHopr>,
    pub expected_messages: u64,
    pub min_guaranteed_messages: u64,
    pub byte_capacity: u64,
}

/// Every entity holding a wxHOPR stake.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapacityAllocations {
    /// Open outgoing channels, keyed by peer.
    pub peer_allocations: BTreeMap<Address, Capacity>,
    /// wxHOPR on the node account, not yet swept into the Safe.
    pub node: Capacity,
    /// The unallocated wxHOPR in the Safe.
    pub safe: Capacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BalanceRecommendation {
    /// Total wxHOPR to fund: channel stakes plus the fee to start.
    pub wxhopr: Balance<WxHopr>,
    /// The total xDai to fund the node with for gas.
    pub xdai: Balance<XDai>,
    pub channel_stakes: Balance<WxHopr>,
    /// One-time key-binding fee; zero once the key is bound on-chain.
    pub fee_to_start: Balance<WxHopr>,
    /// On-chain transactions still needed before channel funding can begin.
    pub txs_to_start: u64,
    /// A ceiling on a single transaction, not expected spend.
    pub xdai_fee_per_tx: Balance<XDai>,
    /// wxHOPR the next top-up pass will try to spend; not part of `wxhopr`.
    pub topup_headroom: Balance<WxHopr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FundingStatus {
    pub traffic: FundingLevel,
    pub gas: FundingLevel,
    /// wxHOPR still needed to reach the ideal recommendation.
    pub wxhopr_deficit: Option<Balance<WxHopr>>,
    /// xDai still needed to reach the ideal recommendation.
    pub xdai_deficit: Option<Balance<XDai>>,
    /// wxHOPR the Safe lacks for the next top-up pass; informational, never moves `traffic`.
    pub refill_shortfall: Option<Balance<WxHopr>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FundingLevel {
    Good,
    Low,
    Empty,
}

// ---- nerd stats ----

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NerdStats {
    NoInfo {
        tickets: TicketStatsStatus,
    },
    Connecting {
        tickets: TicketStatsStatus,
        conn: ConnStats,
    },
    Connected {
        tickets: TicketStatsStatus,
        conn: ConnStats,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TicketStatsStatus {
    Available(TicketStats),
    Waiting,
    Error { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TicketStats {
    pub ticket_price: Balance<WxHopr>,
    pub winning_probability: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnStats {
    pub node_address: Address,
    pub destination: Destination,
    pub wg_pubkey: Option<WgPublicKey>,
    pub wg_server_pubkey: Option<WgPublicKey>,
    /// The tunnel's own address; always a /32.
    pub wg_ip: Option<Ipv4Addr>,
    pub bridge_session: Option<ActiveSession>,
    pub main_session: Option<ActiveSession>,
    pub wg_stats: Option<WgTunnelStats>,
    pub bridge_surb: Option<SurbStats>,
    pub main_surb: Option<SurbStats>,
    pub surb_setpoint_applied: Option<SurbBalancerSetpoint>,
    pub surb_setpoint_target: Option<SurbBalancerSetpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveSession {
    pub kind: SessionKind,
    pub id: SessionId,
    pub bound_host: SocketAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionKind {
    Bridge,
    Ping,
    Main,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SurbStats {
    pub buffer_estimate: Option<u64>,
    pub target_buffer: Option<u64>,
    pub rate_per_sec: Option<f64>,
    pub produced: Option<u64>,
    pub consumed: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SurbBalancerSetpoint {
    pub target_surb_buffer_size: u64,
    pub max_surbs_per_sec: u64,
}

/// The latest tunnel sample plus a bounded window of history, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WgTunnelStats {
    pub current: Option<TunnelStatsSample>,
    pub history: Vec<TunnelStatsSample>,
}

/// `rtt_ms` and `time_since_last_handshake` only advance on a WireGuard handshake, about every two minutes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TunnelStatsSample {
    #[serde(with = "unix_ms")]
    pub at: SystemTime,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub rtt_ms: Option<u32>,
    #[serde(with = "duration_ms::option")]
    pub time_since_last_handshake: Option<Duration>,
    pub estimated_loss: f32,
}

impl Default for TunnelStatsSample {
    fn default() -> Self {
        Self {
            at: SystemTime::UNIX_EPOCH,
            tx_bytes: 0,
            rx_bytes: 0,
            rtt_ms: None,
            time_since_last_handshake: None,
            estimated_loss: 0.0,
        }
    }
}
