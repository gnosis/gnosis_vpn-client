use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::net;
use std::time::{Duration, SystemTime};

use crate::connection::destination::Destination;
use crate::connection::options::SurbConfigError;
use crate::gvpn_client::Registration;
use crate::hopr::HoprError;
use crate::hopr::types::SessionClientMetadata;
use crate::wireguard::WireGuard;
use crate::{gvpn_client, log_output, remote_data, wireguard};

pub mod runner;

#[derive(Debug)]
pub enum Event {
    Progress(Box<Progress>),
    Setback(Box<Setback>),
}

#[derive(Clone, Debug)]
pub enum SessionKind {
    Ping,
    Main,
}

#[derive(Clone, Debug)]
pub enum Progress {
    ResolveBlokliIps,
    GenerateWg(Vec<net::Ipv4Addr>),
    OpenBridge(WireGuard),
    BridgeOpened(SessionClientMetadata),
    RegisterWg,
    OpenPing(Registration),
    BridgeClosed,
    PeerIps,
    KillswitchLockdown,
    StaticWgTunnel(SessionClientMetadata),
    Ping,
    AdjustToMain(Duration),
}

#[derive(Debug)]
pub enum Setback {
    OpenBridge(String),
    RegisterWg(String),
    OpenPing(String),
    Ping(String),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Gvpn client error: {0}")]
    GvpnClient(#[from] gvpn_client::Error),
    #[error("Ping error: {0}")]
    Ping(String),
    #[error("Critical error: {0}")]
    Runtime(String),
    #[error("Surb config error: {0}")]
    SurbConfig(#[from] SurbConfigError),
    #[error("Routing error: {0}")]
    Routing(String),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("Remote data error: {0}")]
    RemoteData(#[from] remote_data::Error),
}

/// Contains stateful data of establishing a VPN connection to a destination.
/// The state transition runner for this struct is in `core::connection::up::runner`.
/// This decision was made to keep all relevant application state accessible in `core`.
/// And avoid duplicating structs in both `core` and `connection` modules.
#[derive(Clone, Debug)]
pub struct Up {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wireguard: Option<WireGuard>,
    pub registration: Option<Registration>,
    /// Temporary bridge session used during key registration; cleared once the background close completes.
    pub bridge_session: Option<SessionClientMetadata>,
    /// The ping session while connecting, promoted to Main once connected.
    pub ping_session: Option<(SessionKind, SessionClientMetadata)>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Phase {
    Init,
    ResolvingBlokliIps,
    GeneratingWg,
    OpeningBridge,
    RegisterWg,
    OpeningPing,
    GatherPeerIps,
    KillswitchLockdown,
    EstablishWgTunnel,
    VerifyPing,
    AdjustToMain,
    ConnectionEstablished,
}

impl Error {
    pub fn is_ping_error(&self) -> bool {
        matches!(self, Error::Ping(_))
    }
}

impl Up {
    pub fn new(destination: Destination) -> Self {
        Self {
            destination,
            phase: (SystemTime::now(), Phase::Init),
            wireguard: None,
            registration: None,
            bridge_session: None,
            ping_session: None,
        }
    }

    pub fn connect_progress(&mut self, evt: Box<Progress>) {
        let now = SystemTime::now();
        match *evt {
            Progress::ResolveBlokliIps => self.phase = (now, Phase::ResolvingBlokliIps),
            Progress::GenerateWg(_) => self.phase = (now, Phase::GeneratingWg),
            Progress::OpenBridge(wg) => {
                self.phase = (now, Phase::OpeningBridge);
                self.wireguard = Some(wg);
            }
            Progress::BridgeOpened(meta) => {
                self.bridge_session = Some(meta);
            }
            Progress::RegisterWg => self.phase = (now, Phase::RegisterWg),
            Progress::OpenPing(reg) => {
                self.phase = (now, Phase::OpeningPing);
                self.registration = Some(reg);
            }
            Progress::BridgeClosed => {
                self.bridge_session = None;
            }
            Progress::PeerIps => self.phase = (now, Phase::GatherPeerIps),
            Progress::KillswitchLockdown => self.phase = (now, Phase::KillswitchLockdown),
            Progress::StaticWgTunnel(session) => {
                self.phase = (now, Phase::EstablishWgTunnel);
                self.ping_session = Some((SessionKind::Ping, session));
            }
            Progress::Ping => self.phase = (now, Phase::VerifyPing),
            Progress::AdjustToMain(_round_trip_time) => self.phase = (now, Phase::AdjustToMain),
        }
    }

    pub fn connected(&mut self) {
        self.phase = (SystemTime::now(), Phase::ConnectionEstablished);
        if let Some((SessionKind::Ping, meta)) = self.ping_session.take() {
            self.ping_session = Some((SessionKind::Main, meta));
        }
    }
}

impl Display for Up {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Connection to {} ({:?} since {})",
            self.destination,
            self.phase.1,
            log_output::elapsed(&self.phase.0)
        )
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase_str = match self {
            Phase::Init => "Init",
            Phase::ResolvingBlokliIps => "Resolving Blokli IPs",
            Phase::GeneratingWg => "Generating WireGuard keypairs",
            Phase::OpeningBridge => "Opening bridge connection",
            Phase::RegisterWg => "Registering WireGuard public key",
            Phase::OpeningPing => "Opening main connection",
            Phase::GatherPeerIps => "Retrieving peer IPs",
            Phase::KillswitchLockdown => "Activating killswitch",
            Phase::EstablishWgTunnel => "Establishing WireGuard tunnel",
            Phase::VerifyPing => "Verifying established connection",
            Phase::AdjustToMain => "Upgrading for general traffic",
            Phase::ConnectionEstablished => "Connection established",
        };
        write!(f, "{}", phase_str)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Progress(p) => write!(f, "Progress: {p}"),
            Event::Setback(s) => write!(f, "Setback: {s}"),
        }
    }
}

impl Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Progress::ResolveBlokliIps => write!(f, "Resolving Blokli IPs"),
            Progress::GenerateWg(_) => write!(f, "Generating WireGuard keypairs"),
            Progress::OpenBridge(_) => write!(f, "Opening bridge connection"),
            Progress::BridgeOpened(_) => write!(f, "Bridge session opened"),
            Progress::RegisterWg => write!(f, "Registering WireGuard public key"),
            Progress::OpenPing(_) => write!(f, "Opening main connection"),
            Progress::BridgeClosed => write!(f, "Bridge session closed"),
            Progress::PeerIps => write!(f, "Retrieving peer IPs"),
            Progress::KillswitchLockdown => write!(f, "Activating killswitch"),
            Progress::StaticWgTunnel(_) => write!(f, "Establishing static WireGuard tunnel"),
            Progress::Ping => write!(f, "Verifying established connection"),
            Progress::AdjustToMain(round_trip_time) => {
                write!(f, "Adjusting to main connection with RTT of {:?}", round_trip_time)
            }
        }
    }
}

impl Display for Setback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Setback::OpenBridge(err) => write!(f, "Failed to open bridge connection: {err}"),
            Setback::RegisterWg(err) => write!(f, "Failed to register WireGuard key: {err}"),
            Setback::OpenPing(err) => write!(f, "Failed to open main connection: {err}"),
            Setback::Ping(err) => write!(f, "Ping verification failed: {err}"),
        }
    }
}
