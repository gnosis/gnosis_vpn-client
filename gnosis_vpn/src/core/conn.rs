use uuid::{self, Uuid};

use std::fmt::{self, Display};

use gnosis_vpn_lib::connection::destination::Destination;

use crate::core::connection_runner;

#[derive(Clone, Debug)]
pub struct Conn {
    pub destination: Destination,
    pub id: Uuid,
    pub phase: Phase,
    pub wg_pub_key: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Phase {
    Init,
    OpeningBridge,
    RegisterWg,
    ClosingBridge,
    OpeningPing,
    EstablishWgTunnel,
    VerifyPing,
    AdjustToMain,
}

impl Conn {
    pub fn new(destination: Destination) -> Self {
        Self {
            destination,
            id: Uuid::new_v4(),
            phase: Phase::Init,
            wg_pub_key: None,
        }
    }

    pub fn on_evt(&mut self, evt: connection_runner::Evt) {
        match evt {
            connection_runner::Evt::OpenBridge => self.phase = Phase::OpeningBridge,
            connection_runner::Evt::Register(wg_pub_key) => {
                self.phase = Phase::RegisterWg;
                self.wg_pub_key = Some(wg_pub_key);
            }
            connection_runner::Evt::CloseBridge => self.phase = Phase::ClosingBridge,
            connection_runner::Evt::OpenPing => self.phase = Phase::OpeningPing,
            connection_runner::Evt::WgTunnel => self.phase = Phase::EstablishWgTunnel,
            connection_runner::Evt::Ping => self.phase = Phase::VerifyPing,
            connection_runner::Evt::AdjustToMain => self.phase = Phase::AdjustToMain,
        }
    }
}

impl Display for Conn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Conn {{ id: {}, destination: {:?}, phase: {:?} }}",
            self.id, self.destination, self.phase
        )
    }
}
