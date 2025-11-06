use std::fmt::{self, Display};
use std::time::SystemTime;

use gnosis_vpn_lib::connection::destination::Destination;
use gnosis_vpn_lib::{log_output, wg_tooling};

use crate::core::connection_runner;

#[derive(Clone, Debug)]
pub struct Conn {
    pub destination: Destination,
    pub phase: (SystemTime, Phase),
    pub wg_public_key: Option<String>,
    pub wg: Option<wg_tooling::WireGuard>,
}

#[derive(Clone, Debug)]
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

impl Conn {
    pub fn new(destination: Destination) -> Self {
        Self {
            destination,
            phase: (SystemTime::now(), Phase::Init),
            wg_public_key: None,
            wg: None,
        }
    }

    pub fn connect_evt(&mut self, evt: connection_runner::Evt) {
        let now = SystemTime::now();
        match evt {
            connection_runner::Evt::GenerateWg => self.phase = (now, Phase::GeneratingWg),
            connection_runner::Evt::OpenBridge => self.phase = (now, Phase::OpeningBridge),
            connection_runner::Evt::RegisterWg(wg_public_key) => {
                self.phase = (now, Phase::RegisterWg);
                self.wg_public_key = Some(wg_public_key);
            }
            connection_runner::Evt::CloseBridge => self.phase = (now, Phase::ClosingBridge),
            connection_runner::Evt::OpenPing => self.phase = (now, Phase::OpeningPing),
            connection_runner::Evt::WgTunnel(wg) => {
                self.wg = Some(wg);
                self.phase = (now, Phase::EstablishWgTunnel);
            }
            connection_runner::Evt::Ping => self.phase = (now, Phase::VerifyPing),
            connection_runner::Evt::AdjustToMain => self.phase = (now, Phase::AdjustToMain),
        }
    }

    pub fn connected(&mut self) {
        self.phase = (SystemTime::now(), Phase::ConnectionEstablished);
    }
}

impl Display for Conn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Conn to {} ({:?} since {})",
            self.destination,
            self.phase.1,
            log_output::elapsed(&self.phase.0)
        )
    }
}
