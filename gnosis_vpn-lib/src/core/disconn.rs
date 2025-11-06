use std::fmt::{self, Display};
use std::time::SystemTime;

use crate::connection::destination::Destination;
use crate::core::conn::Conn;
use crate::core::disconnection_runner;
use crate::{log_output, wg_tooling};

#[derive(Clone, Debug)]
pub struct Disconn {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
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
}

impl TryFrom<&Conn> for Disconn {
    type Error = &'static str;

    fn try_from(value: &Conn) -> Result<Self, Self::Error> {
        if let Some(wg_public_key) = value.wg_public_key.clone() {
            Ok(Self {
                destination: value.destination.clone(),
                phase: (SystemTime::now(), Phase::Disconnecting),
                wg_public_key,
                wg: value.wg.clone(),
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
        let now = SystemTime::now();
        match evt {
            disconnection_runner::Evt::DisconnectWg => self.phase = (now, Phase::DisconnectingWg),
            disconnection_runner::Evt::OpenBridge => self.phase = (now, Phase::DiscOpeningBridge),
            disconnection_runner::Evt::UnregisterWg => self.phase = (now, Phase::UnregisterWg),
            disconnection_runner::Evt::CloseBridge => self.phase = (now, Phase::DiscClosingBridge),
        }
    }
}

impl Display for Disconn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Disconn from {} ({:?} since {})",
            self.destination,
            self.phase.1,
            log_output::elapsed(&self.phase.0)
        )
    }
}
