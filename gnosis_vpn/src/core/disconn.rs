use std::fmt::{self, Display};

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::wg_tooling;

use crate::core::conn::Conn;
use crate::core::disconnection_runner;

#[derive(Clone, Debug)]
pub struct Disconn {
    pub destination: Destination,
    pub phase: Phase,
    pub wg_public_key: String,
    wg: Option<wg_tooling::WireGuard>,
}

#[derive(Clone, Debug)]
pub enum Phase {
    Disconnecting,
    DisconnectingWg,
    DiscOpeningBridge,
    UnregisterWg,
    DiscClosingBridge,
    Disconnected,
}

impl TryFrom<Conn> for Disconn {
    type Error = &'static str;

    fn try_from(value: Conn) -> Result<Self, Self::Error> {
        if let Some(wg_public_key) = value.wg_public_key {
            Ok(Self {
                destination: value.destination,
                phase: Phase::Disconnecting,
                wg_public_key,
                wg: value.wg,
            })
        } else {
            Err("Cannot convert Conn to Disconn: missing WireGuard public key")
        }
    }
}

impl Disconn {
    pub fn wg(&self) -> Option<wg_tooling::WireGuard> {
        self.wg.clone()
    }

    pub fn disconnect_evt(&mut self, evt: disconnection_runner::Evt) {
        match evt {
            disconnection_runner::Evt::DisconnectWg => self.phase = Phase::DisconnectingWg,
            disconnection_runner::Evt::OpenBridge => self.phase = Phase::DiscOpeningBridge,
            disconnection_runner::Evt::UnregisterWg => self.phase = Phase::UnregisterWg,
            disconnection_runner::Evt::CloseBridge => self.phase = Phase::DiscClosingBridge,
        }
    }

    pub fn disconnected(&mut self) {
        self.phase = Phase::Disconnected;
    }
}

impl Display for Disconn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Disconn to {}", self.destination)
    }
}
