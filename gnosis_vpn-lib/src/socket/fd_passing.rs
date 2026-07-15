//! Passing a single file descriptor between the root and worker processes over a
//! dedicated `AF_UNIX` socket using `SCM_RIGHTS` ancillary data.
//!
//! The NepTUN data plane lives in the unprivileged worker, but only root may
//! create the TUN device. Root opens the TUN, then hands the raw fd to the worker
//! so the worker can drive it with `Tunn`. This must travel on a socket *separate*
//! from the newline-JSON worker<->root channel: that channel is read through a
//! `BufReader`, which buffers bytes past a newline that a raw `recvmsg` for the
//! ancillary data would then miss. A dedicated socket only ever does
//! `sendmsg`/`recvmsg`, so there is no framing hazard - ordering reduces to a
//! simple happens-before: root sends the fd first, then reports `TunnelReady` on the
//! JSON channel; the worker waits for `TunnelReady` before `recv_fd`ing here.
//!
//! `recv_fd` returns an [`OwnedFd`] so a decode error or an early drop never leaks
//! the descriptor, and it forces close-on-exec (via `MSG_CMSG_CLOEXEC` on Linux, a
//! post-receipt `fcntl` on platforms without the flag) so a received TUN fd is
//! never inherited by an unrelated spawned child.

#![deny(unsafe_code)]

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags,
};

/// Send a single open file descriptor to the connected peer over `sock`.
///
/// A one-byte regular payload accompanies the ancillary data: `SCM_RIGHTS`
/// requires at least one byte of ordinary data for the control message to be
/// transmitted. The caller retains ownership of `fd`; the kernel duplicates it
/// into the receiver, so `fd` remains valid here and should be closed by the
/// caller as usual.
pub fn send_fd(sock: &UnixStream, fd: &impl AsFd) -> io::Result<()> {
    let payload = [0];
    let iov = [IoSlice::new(&payload)];
    let fds = [fd.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("no room for SCM_RIGHTS control message"));
    }
    let sent = rustix::net::sendmsg(sock, &iov, &mut control, SendFlags::empty()).map_err(io::Error::from)?;
    if sent != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "fd-passing socket did not send its complete payload",
        ));
    }
    Ok(())
}

/// Receive a single file descriptor from the connected peer over `sock`, blocking
/// until one arrives.
///
/// Returns an [`OwnedFd`] that closes on drop, so any error decoding the control
/// message - or an early return by the caller - cannot leak the descriptor. The
/// returned fd is close-on-exec.
pub fn recv_fd(sock: &UnixStream) -> io::Result<OwnedFd> {
    recv_one_fd(sock, RecvFlags::empty())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "no descriptor available"))
}

/// Non-blocking variant of [`recv_fd`]: returns `Ok(None)` when no descriptor is
/// currently buffered on `sock`, instead of blocking. Used to drain descriptors
/// left behind by aborted connection attempts (see [`recv_latest_fd`]).
pub fn try_recv_fd(sock: &UnixStream) -> io::Result<Option<OwnedFd>> {
    recv_one_fd(sock, RecvFlags::DONTWAIT)
}

/// Receive the most recent descriptor buffered on `sock`, discarding (and closing)
/// any older ones.
///
/// The fd-passing socket is long-lived and shared across every in-process
/// reconnect, and `SCM_RIGHTS` on a stream socket delivers descriptors strictly
/// FIFO. If a connection attempt is aborted (disconnect, forced reconnect, or the
/// setup timeout firing) *after* root has sent its TUN fd but *before* the worker
/// consumed it, that fd stays buffered. Because connection bring-up is sequential -
/// the next `SetupTunnel` is not requested until this receive completes - no
/// descriptor for a *later* connection can be queued ahead of the current one. Any
/// surplus descriptors are therefore strictly older orphans, so we block for the
/// first, drain the rest non-blocking, and keep only the last. Draining also closes
/// the orphaned fds, releasing the zombie TUN devices they were keeping alive.
pub fn recv_latest_fd(sock: &UnixStream) -> io::Result<OwnedFd> {
    let mut fd = recv_fd(sock)?;
    let mut discarded = 0u32;
    // Reassigning `fd` drops the previously held OwnedFd, closing the stale fd it
    // superseded.
    while let Some(newer) = try_recv_fd(sock)? {
        discarded += 1;
        fd = newer;
    }
    if discarded > 0 {
        tracing::warn!(
            discarded,
            "discarded stale TUN fd(s) buffered by aborted connection attempt(s); kept the newest"
        );
    }
    Ok(fd)
}

/// Core `recvmsg` for one `SCM_RIGHTS` descriptor. With `MSG_DONTWAIT` in
/// `extra_flags`, a would-block condition (nothing buffered) returns `Ok(None)`
/// rather than an error; every other failure is a hard error.
fn recv_one_fd(sock: &UnixStream, extra_flags: RecvFlags) -> io::Result<Option<OwnedFd>> {
    let mut byte = [0];
    let mut iov = [IoSliceMut::new(&mut byte)];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    // MSG_CMSG_CLOEXEC atomically marks the received fd close-on-exec on Linux,
    // closing the small window where a concurrent spawn could leak it. macOS has
    // no such flag, so it is set with fcntl immediately after receipt below.
    #[cfg(target_os = "linux")]
    let flags = RecvFlags::CMSG_CLOEXEC | extra_flags;
    #[cfg(not(target_os = "linux"))]
    let flags = extra_flags;

    let message = match rustix::net::recvmsg(sock, &mut iov, &mut control, flags) {
        Ok(message) => message,
        Err(error) => {
            let error = io::Error::from(error);
            // A non-blocking drain that finds nothing buffered is not an error.
            if extra_flags.contains(RecvFlags::DONTWAIT) && error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error);
        }
    };
    if message.bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed the fd-passing socket before sending a descriptor",
        ));
    }
    // A truncated control message means the kernel dropped (and closed) fds that
    // did not fit; treat it as a hard error rather than silently proceeding.
    if message.flags.contains(ReturnFlags::CTRUNC) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SCM_RIGHTS control message was truncated",
        ));
    }

    let mut received = None;
    let mut count = 0;
    for ancillary in control.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = ancillary {
            for fd in fds {
                count += 1;
                if received.is_none() {
                    received = Some(fd);
                }
            }
        }
    }
    if count != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected exactly one SCM_RIGHTS file descriptor",
        ));
    }
    let owned = received.expect("one descriptor was counted");

    #[cfg(not(target_os = "linux"))]
    set_cloexec(&owned)?;

    Ok(Some(owned))
}

/// Force `FD_CLOEXEC` on a freshly received descriptor for platforms lacking
/// `MSG_CMSG_CLOEXEC` (e.g. macOS).
#[cfg(not(target_os = "linux"))]
fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    let flags = rustix::io::fcntl_getfd(fd).map_err(io::Error::from)?;
    rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC).map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A fresh anonymous pipe, as two owned ends. The read end is the fd we send
    /// across the socket in tests; writing to the write end and reading it back
    /// through the *received* fd proves the descriptor really was transferred.
    fn make_pipe() -> (OwnedFd, OwnedFd) {
        rustix::pipe::pipe().expect("pipe() failed")
    }

    fn is_cloexec(fd: &OwnedFd) -> bool {
        rustix::io::fcntl_getfd(fd)
            .expect("fcntl F_GETFD failed")
            .contains(rustix::io::FdFlags::CLOEXEC)
    }

    fn send_fds(sock: &UnixStream, fds: &[&OwnedFd]) {
        let payload = [0];
        let iov = [IoSlice::new(&payload)];
        let fds = fds.iter().map(|fd| fd.as_fd()).collect::<Vec<_>>();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(16))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        assert!(control.push(SendAncillaryMessage::ScmRights(&fds)));
        assert_eq!(
            rustix::net::sendmsg(sock, &iov, &mut control, SendFlags::empty()).unwrap(),
            payload.len()
        );
    }

    #[test]
    fn send_and_recv_transfers_a_working_fd() {
        let (a, b) = UnixStream::pair().unwrap();
        let (pipe_r, pipe_w) = make_pipe();

        send_fd(&a, &pipe_r).unwrap();
        let received = recv_fd(&b).unwrap();

        // The sender may now drop its copy of the read end; the transferred fd is
        // an independent kernel reference and must keep the pipe readable.
        drop(pipe_r);

        let mut writer = std::fs::File::from(pipe_w);
        writer.write_all(b"neptun").unwrap();
        drop(writer); // EOF after the payload

        let mut reader = std::fs::File::from(received);
        let mut got = String::new();
        reader.read_to_string(&mut got).unwrap();
        assert_eq!(got, "neptun");
    }

    #[test]
    fn received_fd_is_close_on_exec() {
        let (a, b) = UnixStream::pair().unwrap();
        let (pipe_r, _pipe_w) = make_pipe();
        send_fd(&a, &pipe_r).unwrap();
        let received = recv_fd(&b).unwrap();
        assert!(
            is_cloexec(&received),
            "a received fd must be close-on-exec so it is not leaked across a spawn"
        );
    }

    #[test]
    fn recv_fd_without_ancillary_data_errors() {
        let (a, b) = UnixStream::pair().unwrap();
        // Send a plain byte with no control message.
        (&a).write_all(b"x").unwrap();
        let err = recv_fd(&b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn recv_fd_rejects_multiple_descriptors() {
        let (a, b) = UnixStream::pair().unwrap();
        let (first, _first_writer) = make_pipe();
        let (second, _second_writer) = make_pipe();
        send_fds(&a, &[&first, &second]);
        let err = recv_fd(&b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "expected exactly one SCM_RIGHTS file descriptor");
    }

    #[test]
    fn recv_fd_rejects_truncated_ancillary_data() {
        let (a, b) = UnixStream::pair().unwrap();
        let pipes = (0..16).map(|_| make_pipe()).collect::<Vec<_>>();
        let fds = pipes.iter().map(|(reader, _writer)| reader).collect::<Vec<_>>();
        send_fds(&a, &fds);
        let err = recv_fd(&b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "SCM_RIGHTS control message was truncated");
    }

    #[test]
    fn recv_fd_on_closed_peer_reports_eof() {
        let (a, b) = UnixStream::pair().unwrap();
        drop(a); // peer gone without sending anything
        let err = recv_fd(&b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn send_fd_to_closed_peer_errors_without_consuming_the_fd() {
        let (a, b) = UnixStream::pair().unwrap();
        drop(b); // no receiver
        let (pipe_r, _pipe_w) = make_pipe();
        let err = send_fd(&a, &pipe_r).unwrap_err();
        assert!(
            matches!(err.kind(), io::ErrorKind::BrokenPipe),
            "expected broken pipe, got {:?}",
            err.kind()
        );
        // The caller still owns a valid fd: fcntl on it must succeed.
        rustix::io::fcntl_getfd(&pipe_r).expect("sender must retain the descriptor");
    }

    #[test]
    fn try_recv_fd_returns_none_when_nothing_is_buffered() {
        // Keep `_a` alive so the peer is open (a closed peer would report EOF, not
        // would-block). With nothing sent, a non-blocking receive must not block.
        let (_a, b) = UnixStream::pair().unwrap();
        assert!(try_recv_fd(&b).unwrap().is_none());
    }

    /// Read the whole contents readable through `fd` (its write end must be closed).
    fn read_all(fd: OwnedFd) -> String {
        let mut reader = std::fs::File::from(fd);
        let mut got = String::new();
        reader.read_to_string(&mut got).unwrap();
        got
    }

    #[test]
    fn recv_latest_fd_keeps_the_newest_and_drops_older() {
        let (a, b) = UnixStream::pair().unwrap();
        // Three independent pipes stand in for three connection attempts' TUN fds;
        // the first two are orphans left by aborted attempts.
        let (r1, _w1) = make_pipe();
        let (r2, _w2) = make_pipe();
        let (r3, w3) = make_pipe();
        send_fd(&a, &r1).unwrap();
        send_fd(&a, &r2).unwrap();
        send_fd(&a, &r3).unwrap();

        // Only the newest (r3) survives the drain.
        let received = recv_latest_fd(&b).unwrap();

        // Prove it is r3: data written to w3 is readable through the received fd.
        drop(r3);
        let mut writer = std::fs::File::from(w3);
        writer.write_all(b"newest").unwrap();
        drop(writer);
        assert_eq!(read_all(received), "newest");
    }

    #[test]
    fn recv_latest_fd_returns_the_only_descriptor() {
        let (a, b) = UnixStream::pair().unwrap();
        let (r, w) = make_pipe();
        send_fd(&a, &r).unwrap();
        let received = recv_latest_fd(&b).unwrap();
        drop(r);
        let mut writer = std::fs::File::from(w);
        writer.write_all(b"solo").unwrap();
        drop(writer);
        assert_eq!(read_all(received), "solo");
    }
}
