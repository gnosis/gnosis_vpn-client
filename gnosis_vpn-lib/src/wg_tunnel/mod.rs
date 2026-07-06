//! Embedded WireGuard data plane built on NepTUN's sans-IO `Tunn`.
//!
//! This module owns the userspace WireGuard tunnel that replaces the external
//! `wg`/`wg-quick` tooling. It has two layers:
//!
//! - [`tunnel::WgTunnel`]: a synchronous single-peer WireGuard state machine that
//!   turns packets and datagrams into owned byte buffers - the entire protocol
//!   surface, unit-testable against a second `WgTunnel`.
//! - [`pump::run`]: the async pump that carries packets between the local TUN
//!   device and a network endpoint.
//!
//! The pump's network side is selected by [`data_plane`]: by default a loopback
//! UDP socket connected to the HOPR session bridge port (datagram boundaries
//! guaranteed by UDP), or - opt-in via [`DATA_PLANE_ENV`] - a direct in-process
//! splice of the raw `HoprSession` byte stream with no loopback hop. The splice
//! becomes the default once its frame-boundary assumption (spec risk #1) is
//! validated against a live gvpn server. This module is pure, unprivileged
//! worker-side code; root only provisions the TUN device.

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

/// Environment variable selecting the pump's network endpoint; see [`DataPlane`].
pub const DATA_PLANE_ENV: &str = "GNOSISVPN_WG_DATAPLANE";

/// How the pump's network side reaches the HOPR session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataPlane {
    /// A loopback UDP socket connected to the session bridge port
    /// (`bound_host`). UDP guarantees the one-datagram-per-frame contract the
    /// pump relies on, at the cost of one loopback hop and the bridge's
    /// silent-drop ingress queue.
    UdpBridge,
    /// The raw `HoprSession` spliced directly into the pump - no loopback hop,
    /// no local listener. Relies on the session preserving frame boundaries
    /// (spec risk #1), which must be validated against a live gvpn server
    /// before this becomes the default.
    Splice,
}

/// Read the data-plane selection from [`DATA_PLANE_ENV`].
pub fn data_plane() -> DataPlane {
    data_plane_from(std::env::var(DATA_PLANE_ENV).ok().as_deref())
}

fn data_plane_from(value: Option<&str>) -> DataPlane {
    match value {
        Some("splice") => DataPlane::Splice,
        None | Some("udp-bridge") => DataPlane::UdpBridge,
        Some(other) => {
            tracing::warn!(
                value = %other,
                "unknown {DATA_PLANE_ENV} value; expected 'splice' or 'udp-bridge', using udp-bridge"
            );
            DataPlane::UdpBridge
        }
    }
}

#[cfg(test)]
mod data_plane_tests {
    use super::*;

    #[test]
    fn unset_defaults_to_udp_bridge() {
        assert_eq!(data_plane_from(None), DataPlane::UdpBridge);
    }

    #[test]
    fn splice_is_opt_in() {
        assert_eq!(data_plane_from(Some("splice")), DataPlane::Splice);
    }

    #[test]
    fn udp_bridge_is_accepted_explicitly() {
        assert_eq!(data_plane_from(Some("udp-bridge")), DataPlane::UdpBridge);
    }

    #[test]
    fn unknown_value_falls_back_to_udp_bridge() {
        assert_eq!(data_plane_from(Some("neptun")), DataPlane::UdpBridge);
    }
}

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
