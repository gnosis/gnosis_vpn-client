//! IPC between the root service and the worker process.
//!
//! Two file descriptors are handed to the worker via environment variables at
//! spawn time:
//! - [`ENV_VAR`] carries the newline-JSON control channel (`RootToWorker` /
//!   `WorkerToRoot`).
//! - [`ENV_VAR_TUN_FD`] carries a dedicated `AF_UNIX` socket used only to receive
//!   the TUN device fd from root (via `SCM_RIGHTS`); see
//!   [`crate::socket::fd_passing`]. It is stored process-globally at startup so the
//!   connection runner can pull the fd when root reports the tunnel is ready.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

/// Environment variable holding the raw fd of the newline-JSON control channel.
pub const ENV_VAR: &str = "INTERNAL_WORKER_FD";

/// Environment variable holding the raw fd of the dedicated TUN-fd passing socket.
pub const ENV_VAR_TUN_FD: &str = "INTERNAL_WORKER_TUN_FD";

/// The worker's end of the dedicated TUN-fd passing socket. There is exactly one
/// per worker process, so a process-global avoids threading it through the whole
/// core/runner construction path.
static TUN_FD_SOCKET: OnceLock<Mutex<UnixStream>> = OnceLock::new();

/// Enable or disable close-on-exec for a worker socket descriptor.
pub fn set_cloexec(fd: &impl AsFd, enabled: bool) -> io::Result<()> {
    let mut flags = rustix::io::fcntl_getfd(fd).map_err(io::Error::from)?;
    flags.set(rustix::io::FdFlags::CLOEXEC, enabled);
    rustix::io::fcntl_setfd(fd, flags).map_err(io::Error::from)
}

/// Register the worker's TUN-fd passing socket, taken from [`ENV_VAR_TUN_FD`] at
/// startup. Idempotent: a second call is ignored.
pub fn set_tun_fd_socket(socket: UnixStream) {
    if TUN_FD_SOCKET.set(Mutex::new(socket)).is_err() {
        tracing::warn!("TUN fd socket already initialized; ignoring");
    }
}

/// Block until root sends the TUN device fd over the dedicated socket and return
/// it. Intended to be called from `spawn_blocking` since it performs a blocking
/// `recvmsg`. Errors if the socket was never initialized (worker not spawned by
/// root with [`ENV_VAR_TUN_FD`]).
///
/// Uses [`recv_latest_fd`](super::fd_passing::recv_latest_fd) so that a descriptor
/// orphaned by an aborted connection attempt (setup timeout or cancel firing after
/// root sent the fd but before the worker consumed it) is drained and closed rather
/// than mistaken for the current connection's device.
pub fn recv_tun_fd() -> io::Result<OwnedFd> {
    let cell = TUN_FD_SOCKET
        .get()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "TUN fd socket not initialized"))?;
    let socket = cell
        .lock()
        .map_err(|_| io::Error::other("TUN fd socket lock poisoned"))?;
    super::fd_passing::recv_latest_fd(&socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloexec_can_be_cleared_and_restored() {
        let (socket, _peer) = UnixStream::pair().expect("socket pair");
        let initial = rustix::io::fcntl_getfd(&socket).expect("read initial fd flags");
        assert!(initial.contains(rustix::io::FdFlags::CLOEXEC));

        set_cloexec(&socket, false).expect("clear close-on-exec");
        let cleared = rustix::io::fcntl_getfd(&socket).expect("read cleared fd flags");
        assert!(!cleared.contains(rustix::io::FdFlags::CLOEXEC));

        set_cloexec(&socket, true).expect("restore close-on-exec");
        let restored = rustix::io::fcntl_getfd(&socket).expect("read restored fd flags");
        assert!(restored.contains(rustix::io::FdFlags::CLOEXEC));
    }
}
