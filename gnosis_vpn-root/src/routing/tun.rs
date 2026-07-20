//! Privileged TUN device creation for the NepTUN data plane.
//!
//! Root creates the device and hands its fd to the worker (see
//! `gnosis_vpn_lib::socket::fd_passing`); the worker drives it with `Tunn`.
//! Cross-platform device creation - the `/dev/net/tun` + `TUNSETIFF` dance on Linux
//! and the `utun` control-socket dance on macOS - is delegated to NepTUN's
//! `TunSocket` so the platform ioctls are shared, upstream-tested code. Address,
//! MTU and link-state assignment is platform-specific and lives in the platform
//! routers.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

use neptun::device::tun::TunSocket;

use super::Error;

/// A TUN device owned by root. Holding the fd keeps the interface alive (a TUN
/// vanishes when its last fd closes); dropping it on teardown closes root's fd.
/// The worker receives an independent dup of the fd via `SCM_RIGHTS`, so the
/// interface persists until both processes drop their fds.
pub struct Tun {
    fd: OwnedFd,
    name: String,
}

impl Tun {
    /// Create a TUN device. On Linux `requested_name` is used verbatim (e.g.
    /// `wg0_gnosisvpn`); on macOS pass `utun` and the kernel assigns a `utunN`
    /// name, read back via [`Tun::name`]. Requires root / `CAP_NET_ADMIN`.
    pub fn create(requested_name: &str) -> Result<Self, Error> {
        let socket = TunSocket::new(requested_name).map_err(|e| Error::Tun(format!("create {requested_name}: {e}")))?;
        let name = socket
            .name()
            .map_err(|e| Error::Tun(format!("resolve interface name: {e}")))?;
        // NepTUN's TunSocket exposes its descriptor only as a bare RawFd. Dup it
        // into an OwnedFd and drop the TunSocket, so fd ownership is typed from
        // here on: the duplicate refers to the same open device (keeping the
        // interface alive) and gains CLOEXEC, which the original lacks.
        // SAFETY: `socket` owns the descriptor and stays alive until `drop(socket)`
        // below, so the borrow cannot outlive an open fd.
        let borrowed = unsafe { BorrowedFd::borrow_raw(socket.as_raw_fd()) };
        let fd =
            rustix::io::fcntl_dupfd_cloexec(borrowed, 0).map_err(|e| Error::Tun(format!("dup fd of {name}: {e}")))?;
        drop(socket);
        tracing::info!(interface = %name, "created TUN device");
        Ok(Self { fd, name })
    }

    /// The kernel-resolved interface name (`wg0_gnosisvpn` on Linux, `utunN` on
    /// macOS).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl AsFd for Tun {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
