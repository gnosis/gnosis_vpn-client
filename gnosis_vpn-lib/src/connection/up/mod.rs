use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

use crate::connection::destination::Destination;
use crate::core::runner::SurbConfigError;
use crate::gvpn_client::Registration;
use crate::hopr::HoprError;
use crate::hopr::types::SessionClientMetadata;
use crate::wireguard::WireGuard;
use crate::{gvpn_client, log_output, wireguard};

pub mod runner;

#[derive(Debug)]
pub enum Event {
    Progress(Progress),
    Setback(Setback),
}

#[derive(Debug)]
pub enum Progress {
    GenerateWg,
    OpenBridge(WireGuard),
    RegisterWg,
    CloseBridge(Registration),
    OpenPing,
    DynamicWgTunnel(SessionClientMetadata),
    PeerIps,
    StaticWgTunnel(usize),
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
}

/// Contains stateful data of establishing a VPN connection to a destination.
/// The state transition runner for this struct is in `core::connection::up::runner`.
/// This decision was made to keep all relevant application state accessible in `core`.
/// And avoid duplicating structs in both `core` and `connection` modules.
#[derive(Clone, Debug)]
pub struct Up {
    // TODO phase out this struct of in between storage
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wireguard: Option<WireGuard>,
    pub registration: Option<Registration>,
    pub session: Option<SessionClientMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Phase {
    Init,
    GeneratingWg,
    OpeningBridge,
    RegisterWg,
    ClosingBridge,
    OpeningPing,
    EstablishDynamicWgTunnel,
    FallbackGatherPeerIps,
    FallbackToStaticWgTunnel,
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
            session: None,
        }
    }

    pub fn connect_progress(&mut self, evt: Progress) {
        let now = SystemTime::now();
        match evt {
            Progress::GenerateWg => self.phase = (now, Phase::GeneratingWg),
            Progress::OpenBridge(wg) => {
                self.phase = (now, Phase::OpeningBridge);
                self.wireguard = Some(wg);
            }
            Progress::RegisterWg => self.phase = (now, Phase::RegisterWg),
            Progress::CloseBridge(reg) => {
                self.phase = (now, Phase::ClosingBridge);
                self.registration = Some(reg);
            }
            Progress::OpenPing => self.phase = (now, Phase::OpeningPing),
            Progress::DynamicWgTunnel(session) => {
                self.phase = (now, Phase::EstablishDynamicWgTunnel);
                self.session = Some(session);
            }
            Progress::PeerIps => self.phase = (now, Phase::FallbackGatherPeerIps),
            Progress::StaticWgTunnel(_announced_peer_count) => {
                self.phase = (now, Phase::FallbackToStaticWgTunnel);
            }
            Progress::Ping => self.phase = (now, Phase::VerifyPing),
            Progress::AdjustToMain(_round_trip_time) => self.phase = (now, Phase::AdjustToMain),
        }
    }

    pub fn connected(&mut self) {
        self.phase = (SystemTime::now(), Phase::ConnectionEstablished);
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
            Phase::GeneratingWg => "Generating WireGuard keypairs",
            Phase::OpeningBridge => "Opening bridge connection",
            Phase::RegisterWg => "Registering WireGuard public key",
            Phase::ClosingBridge => "Closing bridge connection",
            Phase::OpeningPing => "Opening main connection",
            Phase::EstablishDynamicWgTunnel => "Establishing dynamically routed WireGuard tunnel",
            Phase::FallbackGatherPeerIps => "Retrieving peer IPs for static tunnel",
            Phase::FallbackToStaticWgTunnel => "Establishing statically routed WireGuard tunnel",
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
            Progress::GenerateWg => write!(f, "Generating WireGuard keypairs"),
            Progress::OpenBridge(_) => write!(f, "Opening bridge connection"),
            Progress::RegisterWg => write!(f, "Registering WireGuard public key"),
            Progress::CloseBridge(_) => write!(f, "Closing bridge connection"),
            Progress::OpenPing => write!(f, "Opening main connection"),
            Progress::DynamicWgTunnel(_) => write!(f, "Establishing dynamic WireGuard tunnel"),
            Progress::PeerIps => write!(f, "Retrieving peer IPs"),
            Progress::StaticWgTunnel(announced_peer_count) => write!(
                f,
                "Establishing static WireGuard tunnel with {announced_peer_count} announced peers"
            ),
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
