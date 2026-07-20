//! IPC between the root service and the worker process.
//!
//! Two file descriptors reach the worker at spawn time:
//! - The newline-JSON control channel (`RootToWorker` / `WorkerToRoot`) arrives as
//!   the worker's stdin; [`claim_stdin_socket`] validates and adopts it.
//! - A dedicated `AF_UNIX` socket used only to receive the TUN device fd from root
//!   (via `SCM_RIGHTS`; see [`crate::socket::fd_passing`]) arrives as the first
//!   message on the control channel, ahead of any JSON traffic. It is stored
//!   process-globally at startup so the connection runner can pull the fd when
//!   root reports the tunnel is ready.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

/// The worker's end of the dedicated TUN-fd passing socket. There is exactly one
/// per worker process, so a process-global avoids threading it through the whole
/// core/runner construction path.
static TUN_FD_SOCKET: OnceLock<Mutex<UnixStream>> = OnceLock::new();

/// Claim the control socket that root passed as the worker's stdin.
///
/// Verifies fd 0 really is an `AF_UNIX` socket (a manual launch from a terminal
/// fails fast here), adopts an owned duplicate of it, and repoints fd 0 at
/// `/dev/null` so no child process spawned later can hold the control socket open.
pub fn claim_stdin_socket() -> io::Result<UnixStream> {
    let owned = io::stdin().as_fd().try_clone_to_owned()?;
    let socket = unix_stream_from(owned)?;
    let devnull = std::fs::File::open("/dev/null")?;
    rustix::stdio::dup2_stdin(&devnull).map_err(io::Error::from)?;
    Ok(socket)
}

/// The fd as a `UnixStream`, after verifying it is an `AF_UNIX` socket.
fn unix_stream_from(owned: OwnedFd) -> io::Result<UnixStream> {
    let addr = rustix::net::getsockname(&owned).map_err(io::Error::from)?;
    if addr.address_family() != rustix::net::AddressFamily::UNIX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fd is not an AF_UNIX socket",
        ));
    }
    Ok(UnixStream::from(owned))
}

/// Register the worker's TUN-fd passing socket, received over the control socket
/// at startup. Idempotent: a second call is ignored.
pub fn set_tun_fd_socket(socket: UnixStream) {
    if TUN_FD_SOCKET.set(Mutex::new(socket)).is_err() {
        tracing::warn!("TUN fd socket already initialized; ignoring");
    }
}

/// Block until root sends the TUN device fd over the dedicated socket and return
/// it. Intended to be called from `spawn_blocking` since it performs a blocking
/// `recvmsg`. Errors if the socket was never initialized (worker not spawned by
/// root).
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
    use std::io::{Read, Write};

    #[test]
    fn unix_stream_from_accepts_a_unix_socket() {
        let (a, mut b) = UnixStream::pair().expect("socket pair");
        let mut adopted = unix_stream_from(OwnedFd::from(a)).expect("an AF_UNIX socket must be accepted");

        b.write_all(b"ping").expect("write to peer");
        let mut buf = [0u8; 4];
        adopted.read_exact(&mut buf).expect("read through adopted socket");
        assert_eq!(&buf, b"ping", "the adopted stream must be the same socket");
    }

    #[test]
    fn unix_stream_from_rejects_a_non_socket_fd() {
        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        let err = unix_stream_from(OwnedFd::from(devnull)).expect_err("a plain file must be rejected");
        assert_eq!(err.raw_os_error(), Some(rustix::io::Errno::NOTSOCK.raw_os_error()));
    }
}
