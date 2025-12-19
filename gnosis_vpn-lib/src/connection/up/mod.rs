use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::connection::destination::Destination;
use crate::gvpn_client::Registration;
use crate::hopr::HoprError;
use crate::hopr::types::SessionClientMetadata;
use crate::wireguard::WireGuard;
use crate::{gvpn_client, log_output, ping, wireguard};

pub mod runner_post_wg;
pub mod runner_pre_wg;

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
    WgTunnel(SessionClientMetadata),
    Ping,
    AdjustToMain,
}

#[derive(Debug)]
pub enum Setback {
    OpenBridge(String),
    RegisterWg(String),
    OpenPing(String),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr error: {0}")]
    Hopr(#[from] HoprError),
    #[error("Gvpn client error: {0}")]
    GvpnClient(#[from] gvpn_client::Error),
    #[error("WireGuard error: {0}")]
    WireGuard(#[from] wireguard::Error),
    #[error("Ping error: {0}")]
    Ping(#[from] ping::Error),
    #[error("Critical error: {0}")]
    Runtime(String),
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
            Progress::WgTunnel(session) => {
                self.phase = (now, Phase::EstablishWgTunnel);
                self.session = Some(session);
            }
            Progress::Ping => self.phase = (now, Phase::VerifyPing),
            Progress::AdjustToMain => self.phase = (now, Phase::AdjustToMain),
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
            Progress::GenerateWg => write!(f, "Generating WireGuard keypairs"),
            Progress::OpenBridge(_) => write!(f, "Opening bridge connection"),
            Progress::RegisterWg => write!(f, "Registering WireGuard public key"),
            Progress::CloseBridge(_) => write!(f, "Closing bridge connection"),
            Progress::OpenPing => write!(f, "Opening main connection"),
            Progress::WgTunnel(_) => write!(f, "Establishing WireGuard tunnel"),
            Progress::Ping => write!(f, "Verifying established connection"),
            Progress::AdjustToMain => write!(f, "Upgrading for general traffic"),
        }
    }
}

impl Display for Setback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Setback::OpenBridge(reason) => write!(f, "Failed to open bridge session: {}", reason),
            Setback::RegisterWg(reason) => write!(f, "Failed to register WireGuard public key: {}", reason),
            Setback::OpenPing(reason) => write!(f, "Failed to open main session: {}", reason),
        }
    }
}
