use bytesize::ByteSize;
use edgli::hopr_lib::SurbBalancerConfig;
use human_bandwidth::re::bandwidth::Bandwidth;
use uuid::{self, Uuid};

use std::fmt::{self, Display};

use gnosis_vpn_lib::connection::destination::Destination;

use crate::core::{connection_runner, disconnection_runner};

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
    ConnectionEstablished,
    Disconnecting,
    DiscOpeningBridge,
    UnregisterWg,
    DiscClosingBridge,
    Disconnected,
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

    pub fn connect_evt(&mut self, evt: connection_runner::Evt) {
        match evt {
            connection_runner::Evt::OpenBridge => self.phase = Phase::OpeningBridge,
            connection_runner::Evt::RegisterWg(wg_pub_key) => {
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

    pub fn connected(&mut self) {
        self.phase = Phase::ConnectionEstablished;
    }

    pub fn to_disconnect(&mut self) {
        match self.phase {}
    }

    pub fn disconnect_evt(&mut self, evt: disconnection_runner::Evt) {
        match evt {
            disconnection_runner::Evt::OpenBridge => self.phase = Phase::DiscOpeningBridge,
            disconnection_runner::Evt::UnregisterWg => self.phase = Phase::UnregisterWg,
            disconnection_runner::Evt::CloseBridge => self.phase = Phase::DiscClosingBridge,
        }
    }

    pub fn disconnected(&mut self) {
        self.phase = Phase::Disconnected;
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

pub fn to_surb_balancer_config(response_buffer: ByteSize, max_surb_upstream: Bandwidth) -> SurbBalancerConfig {
    // Buffer worth at least 2 reply packets
    if response_buffer.as_u64() >= 2 * edgli::hopr_lib::SESSION_MTU as u64 {
        SurbBalancerConfig {
            target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::SESSION_MTU as u64,
            max_surbs_per_sec: (max_surb_upstream.as_bps() as usize / (8 * edgli::hopr_lib::SURB_SIZE)) as u64,
            ..Default::default()
        }
    } else {
        // Use defaults otherwise
        Default::default()
    }
}
