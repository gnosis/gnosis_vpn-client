use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::connection::destination::Destination;
use crate::core::runner::SurbConfigError;
use crate::hopr::HoprError;
use crate::{connection, gvpn_client, log_output, ping};

pub mod runner;

/// Contains stateful data of dismantling a VPN connection from a destination.
/// The state transition runner for this struct is in `core::connection::down::runner`.
/// This decision was made to keep all relevant application state accessible in `core`.
/// And avoid duplicating structs in both `core` and `connection` modules.
#[derive(Clone, Debug)]
pub struct Down {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wg_public_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Phase {
    Disconnecting,
    DisconnectingWg,
    OpeningBridge,
    UnregisterWg,
    ClosingBridge,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Hopr(#[from] HoprError),
    #[error(transparent)]
    GvpnClient(#[from] gvpn_client::Error),
    #[error(transparent)]
    Ping(#[from] ping::Error),
    #[error("Surb config error: {0}")]
    SurbConfig(#[from] SurbConfigError),
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    DisconnectWg,
    OpenBridge,
    UnregisterWg,
    CloseBridge,
}

/// Depending on how far a connection was already established,
/// different steps for dismantling need to be taken.
/// If no wg pubkey was generated, nothing needs to be done to rewind a connection attempt.
impl TryFrom<&connection::up::Up> for Down {
    type Error = &'static str;

    fn try_from(value: &connection::up::Up) -> Result<Self, Self::Error> {
        if let Some(wg) = value.wireguard.clone() {
            Ok(Self {
                destination: value.destination.clone(),
                phase: (SystemTime::now(), Phase::Disconnecting),
                wg_public_key: wg.key_pair.public_key,
            })
        } else {
            Err("Cannot convert Up to Down: missing WireGuard public key")
        }
    }
}

impl Down {
    pub fn disconnect_evt(&mut self, evt: Event) {
        let now = SystemTime::now();
        match evt {
            Event::DisconnectWg => self.phase = (now, Phase::DisconnectingWg),
            Event::OpenBridge => self.phase = (now, Phase::OpeningBridge),
            Event::UnregisterWg => self.phase = (now, Phase::UnregisterWg),
            Event::CloseBridge => self.phase = (now, Phase::ClosingBridge),
        }
    }
}

impl Display for Down {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Disconnection from {} ({:?} since {})",
            self.destination,
            self.phase.1,
            log_output::elapsed(&self.phase.0)
        )
    }
}

impl Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase_str = match self {
            Phase::Disconnecting => "Disconnecting",
            Phase::DisconnectingWg => "Disconnecting WireGuard tunnel",
            Phase::OpeningBridge => "Opening bridge connection",
            Phase::UnregisterWg => "Unregistering WireGuard public key",
            Phase::ClosingBridge => "Closing bridge connection",
        };
        write!(f, "{}", phase_str)
    }
}
