//! Privileged TUN device creation for the NepTUN data plane.
//!
//! Root creates the device and hands its fd to the worker (see
//! `gnosis_vpn_lib::socket::fd_passing`); the worker drives it with `Tunn`.
//! Cross-platform device creation - the `/dev/net/tun` + `TUNSETIFF` dance on Linux
//! and the `utun` control-socket dance on macOS - is delegated to NepTUN's
//! `TunSocket` so the platform ioctls are shared, upstream-tested code. Address,
//! MTU and link-state assignment is platform-specific and lives in the platform
//! routers.

use std::os::fd::{AsRawFd, RawFd};

use neptun::device::tun::TunSocket;

use super::Error;

/// A TUN device owned by root. Holding the [`TunSocket`] keeps the interface alive
/// (a TUN vanishes when its last fd closes); dropping it on teardown closes root's
/// fd. The worker receives an independent dup of the fd via `SCM_RIGHTS`, so the
/// interface persists until both processes drop their fds.
pub struct Tun {
    socket: TunSocket,
    name: String,
}

impl Tun {
    /// Create a TUN device. On Linux `requested_name` is used verbatim (e.g.
    /// `wg0_gnosisvpn`); on macOS pass `utun` and the kernel assigns a `utunN`
    /// name, read back via [`Tun::name`]. Requires root / `CAP_NET_ADMIN`.
    pub fn create(requested_name: &str) -> Result<Self, Error> {
        let socket =
            TunSocket::new(requested_name).map_err(|e| Error::Tun(format!("create {requested_name}: {e}")))?;
        let name = socket.name().map_err(|e| Error::Tun(format!("resolve interface name: {e}")))?;
        tracing::info!(interface = %name, "created TUN device");
        Ok(Self { socket, name })
    }

    /// The kernel-resolved interface name (`wg0_gnosisvpn` on Linux, `utunN` on
    /// macOS).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Root's raw fd for the device. Valid while this `Tun` is alive; used to hand
    /// a dup to the worker via `SCM_RIGHTS`.
    pub fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}
