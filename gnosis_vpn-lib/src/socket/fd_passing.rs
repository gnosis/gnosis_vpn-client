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
//! simple happens-before (root sends `TunnelReady` on the JSON channel first, then
//! the worker `recv_fd`s here).
//!
//! `recv_fd` returns an [`OwnedFd`] so a decode error or an early drop never leaks
//! the descriptor, and it forces close-on-exec (via `MSG_CMSG_CLOEXEC` on Linux, a
//! post-receipt `fcntl` on platforms without the flag) so a received TUN fd is
//! never inherited by an unrelated spawned child.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Backing storage for one `SCM_RIGHTS` control message. `CMSG_SPACE(4)` is well
/// under 32 bytes on every supported platform; 64 with `cmsghdr` alignment leaves
/// generous headroom while satisfying the kernel's alignment requirement.
#[repr(C, align(8))]
struct CmsgBuf([u8; 64]);

impl CmsgBuf {
    fn zeroed() -> Self {
        CmsgBuf([0u8; 64])
    }
}

/// The size, in bytes, of a single `RawFd` payload carried by the control message.
const FD_SIZE: u32 = std::mem::size_of::<RawFd>() as u32;

/// Send a single open file descriptor to the connected peer over `sock`.
///
/// A one-byte regular payload accompanies the ancillary data: `SCM_RIGHTS`
/// requires at least one byte of ordinary data for the control message to be
/// transmitted. The caller retains ownership of `fd`; the kernel duplicates it
/// into the receiver, so `fd` remains valid here and should be closed by the
/// caller as usual.
pub fn send_fd(sock: &UnixStream, fd: RawFd) -> io::Result<()> {
    let payload: [u8; 1] = [0];
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };

    let mut cmsg = CmsgBuf::zeroed();
    // SAFETY: msghdr has private padding on some platforms, so it must be
    // zero-initialized and then filled field by field rather than built with a
    // struct literal.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.0.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(FD_SIZE) } as _;

    // SAFETY: the control buffer is sized for exactly one fd and correctly
    // aligned, so CMSG_FIRSTHDR yields a valid header we fully initialize before
    // copying the descriptor into its data region.
    unsafe {
        let hdr = libc::CMSG_FIRSTHDR(&msg);
        if hdr.is_null() {
            return Err(io::Error::other("no room for SCM_RIGHTS control message"));
        }
        (*hdr).cmsg_level = libc::SOL_SOCKET;
        (*hdr).cmsg_type = libc::SCM_RIGHTS;
        (*hdr).cmsg_len = libc::CMSG_LEN(FD_SIZE) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(hdr),
            FD_SIZE as usize,
        );
    }

    // SAFETY: msg points at live, correctly typed local storage for the duration
    // of the call.
    let ret = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Receive a single file descriptor from the connected peer over `sock`.
///
/// Returns an [`OwnedFd`] that closes on drop, so any error decoding the control
/// message - or an early return by the caller - cannot leak the descriptor. The
/// returned fd is close-on-exec.
pub fn recv_fd(sock: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte: [u8; 1] = [0];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    };

    let mut cmsg = CmsgBuf::zeroed();
    // SAFETY: see send_fd - msghdr must be zeroed then filled field by field.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.0.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(FD_SIZE) } as _;

    // MSG_CMSG_CLOEXEC atomically marks the received fd close-on-exec on Linux,
    // closing the small window where a concurrent spawn could leak it. macOS has
    // no such flag, so it is set with fcntl immediately after receipt below.
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;

    // SAFETY: msg points at live, correctly typed local storage.
    let ret = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, flags) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if ret == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed the fd-passing socket before sending a descriptor",
        ));
    }
    // A truncated control message means the kernel dropped (and closed) fds that
    // did not fit; treat it as a hard error rather than silently proceeding.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SCM_RIGHTS control message was truncated",
        ));
    }

    // SAFETY: recvmsg populated msg; CMSG_FIRSTHDR returns either null or a valid
    // header pointer into our control buffer. We validate level/type/len before
    // reading exactly one fd out of the data region.
    let owned = unsafe {
        let hdr = libc::CMSG_FIRSTHDR(&msg);
        if hdr.is_null()
            || (*hdr).cmsg_level != libc::SOL_SOCKET
            || (*hdr).cmsg_type != libc::SCM_RIGHTS
            || (*hdr).cmsg_len as usize != libc::CMSG_LEN(FD_SIZE) as usize
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected exactly one SCM_RIGHTS file descriptor",
            ));
        }
        let mut fd: RawFd = -1;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(hdr),
            &mut fd as *mut RawFd as *mut u8,
            FD_SIZE as usize,
        );
        if fd < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "received an invalid fd"));
        }
        // Take ownership immediately so every subsequent early return closes it.
        OwnedFd::from_raw_fd(fd)
    };

    #[cfg(not(target_os = "linux"))]
    set_cloexec(&owned)?;

    Ok(owned)
}

/// Force `FD_CLOEXEC` on a freshly received descriptor for platforms lacking
/// `MSG_CMSG_CLOEXEC` (e.g. macOS).
#[cfg(not(target_os = "linux"))]
fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: raw is a live fd owned by `fd` for the duration of these calls.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A fresh anonymous pipe, as two owned ends. The read end is the fd we send
    /// across the socket in tests; writing to the write end and reading it back
    /// through the *received* fd proves the descriptor really was transferred.
    fn make_pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: fds is a valid 2-element array; pipe(2) fills it on success.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed: {}", io::Error::last_os_error());
        // SAFETY: both fds are freshly created and owned by nobody else.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn is_cloexec(fd: &OwnedFd) -> bool {
        // SAFETY: fd is live and owned.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "fcntl F_GETFD failed");
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn send_and_recv_transfers_a_working_fd() {
        let (a, b) = UnixStream::pair().unwrap();
        let (pipe_r, pipe_w) = make_pipe();

        send_fd(&a, pipe_r.as_raw_fd()).unwrap();
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
        send_fd(&a, pipe_r.as_raw_fd()).unwrap();
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
        let err = send_fd(&a, pipe_r.as_raw_fd()).unwrap_err();
        assert!(
            matches!(err.kind(), io::ErrorKind::BrokenPipe),
            "expected broken pipe, got {:?}",
            err.kind()
        );
        // The caller still owns a valid fd: fcntl on it must succeed.
        assert!(unsafe { libc::fcntl(pipe_r.as_raw_fd(), libc::F_GETFD) } >= 0);
    }
}
