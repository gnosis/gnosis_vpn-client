use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::connection::destination::Destination;
use crate::{log_output, wg_tooling};

mod runner;

/// Contains stateful data of establishing a VPN connection to a destination.
/// The state transition runner for this struct is in `core::connection::up::runner`.
/// This decision was made to keep all relevant application state accessible in `core`.
/// And avoid duplicating structs in both `core` and `connection` modules.
#[derive(Clone, Debug)]
pub struct Up {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wg_public_key: Option<String>,
    pub wg: Option<wg_tooling::WireGuard>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

impl Up {
    pub fn new(destination: Destination) -> Self {
        Self {
            destination,
            phase: (SystemTime::now(), Phase::Init),
            wg_public_key: None,
            wg: None,
        }
    }

    pub fn connect_evt(&mut self, evt: runner::Event) {
        let now = SystemTime::now();
        match evt {
            runner::Event::GenerateWg => self.phase = (now, Phase::GeneratingWg),
            runner::Event::OpenBridge => self.phase = (now, Phase::OpeningBridge),
            runner::Event::RegisterWg(wg_public_key) => {
                self.phase = (now, Phase::RegisterWg);
                self.wg_public_key = Some(wg_public_key);
            }
            runner::Event::CloseBridge => self.phase = (now, Phase::ClosingBridge),
            runner::Event::OpenPing => self.phase = (now, Phase::OpeningPing),
            runner::Event::WgTunnel(wg) => {
                self.wg = Some(wg);
                self.phase = (now, Phase::EstablishWgTunnel);
            }
            runner::Event::Ping => self.phase = (now, Phase::VerifyPing),
            runner::Event::AdjustToMain => self.phase = (now, Phase::AdjustToMain),
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
