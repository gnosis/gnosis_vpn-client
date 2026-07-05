//! Embedded WireGuard data plane built on NepTUN's sans-IO `Tunn`.
//!
//! This module owns the userspace WireGuard tunnel that replaces the external
//! `wg`/`wg-quick` tooling. It has two layers:
//!
//! - [`tunnel::WgTunnel`]: a synchronous single-peer WireGuard state machine that
//!   turns packets and datagrams into owned byte buffers - the entire protocol
//!   surface, unit-testable against a second `WgTunnel`.
//! - [`pump::run`]: the async pump that carries packets between the local TUN
//!   device and a `HoprSession` byte stream, with no loopback UDP hop.
//!
//! Root (privileged) TUN provisioning and the session splice that feeds a real
//! `HoprSession` into the pump are wired in later phases; this module is pure,
//! unprivileged worker-side code.

mod pump;
mod session;
#[cfg(unix)]
mod tun;
mod tunnel;
mod udp;

use neptun::noise::errors::WireGuardError;

pub use pump::{NetworkReceiver, NetworkSender, PumpExit, TunReceiver, TunSender, run};
pub use session::{SessionReceiver, SessionSender};
#[cfg(unix)]
pub use tun::{PLATFORM_TUN_HEADER_LEN, TunReader, TunWriter, tun_endpoints};
pub use tunnel::{Outputs, TimerTick, TunnelEngine, WgTunnel};
pub use udp::{UdpReceiver, UdpSender, udp_endpoints};

/// Upper bound on a single IP packet or WireGuard datagram the pump buffers.
/// WG at MTU 1420 yields ciphertext up to ~1452 bytes; 2048 leaves headroom.
pub(crate) const MAX_FRAME: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Key(#[from] crate::wireguard::Error),
    #[error("failed to initialize wireguard tunnel: {0}")]
    Tunn(&'static str),
    #[error("wireguard protocol error: {0:?}")]
    WireGuard(WireGuardError),
    #[error("unexpected tunnel state: {0}")]
    Unexpected(&'static str),
}
