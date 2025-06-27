use libc;
use mnl_sys::{
    self,
    libc::{c_uint, c_void, pid_t},
};
use std::{
    io, mem,
    os::unix::io::{AsRawFd, RawFd},
};

use crate::cvt::cvt;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
#[repr(i32)]
pub enum Bus {
    Route = libc::NETLINK_ROUTE,
    Unused = libc::NETLINK_UNUSED,
    Usersock = libc::NETLINK_USERSOCK,
    Firewall = libc::NETLINK_FIREWALL,
    SockDiag = libc::NETLINK_SOCK_DIAG,
    Nflog = libc::NETLINK_NFLOG,
    Xfrm = libc::NETLINK_XFRM,
    Selinux = libc::NETLINK_SELINUX,
    Iscsi = libc::NETLINK_ISCSI,
    Audit = libc::NETLINK_AUDIT,
    FibLookup = libc::NETLINK_FIB_LOOKUP,
    Connector = libc::NETLINK_CONNECTOR,
    Netfilter = libc::NETLINK_NETFILTER,
    Ip6Fw = libc::NETLINK_IP6_FW,
    Dnrtmsg = libc::NETLINK_DNRTMSG,
    KobjectUevent = libc::NETLINK_KOBJECT_UEVENT,
    Generic = libc::NETLINK_GENERIC,
    Scsitransport = libc::NETLINK_SCSITRANSPORT,
    Ecryptfs = libc::NETLINK_ECRYPTFS,
    Rdma = libc::NETLINK_RDMA,
    Crypto = libc::NETLINK_CRYPTO,
}

impl Bus {
    /// Converts the given integer to a netlink bus variant. Returns `None` if the value does
    /// not match any bus.
    pub fn try_from(bus: i32) -> Option<Self> {
        use crate::Bus::*;
        let variant = match bus {
            libc::NETLINK_ROUTE => Route,
            libc::NETLINK_UNUSED => Unused,
            libc::NETLINK_USERSOCK => Usersock,
            libc::NETLINK_FIREWALL => Firewall,
            libc::NETLINK_SOCK_DIAG => SockDiag,
            libc::NETLINK_NFLOG => Nflog,
            libc::NETLINK_XFRM => Xfrm,
            libc::NETLINK_SELINUX => Selinux,
            libc::NETLINK_ISCSI => Iscsi,
            libc::NETLINK_AUDIT => Audit,
            libc::NETLINK_FIB_LOOKUP => FibLookup,
            libc::NETLINK_CONNECTOR => Connector,
            libc::NETLINK_NETFILTER => Netfilter,
            libc::NETLINK_IP6_FW => Ip6Fw,
            libc::NETLINK_DNRTMSG => Dnrtmsg,
            libc::NETLINK_KOBJECT_UEVENT => KobjectUevent,
            libc::NETLINK_GENERIC => Generic,
            libc::NETLINK_SCSITRANSPORT => Scsitransport,
            libc::NETLINK_ECRYPTFS => Ecryptfs,
            libc::NETLINK_RDMA => Rdma,
            libc::NETLINK_CRYPTO => Crypto,
            _ => return None,
        };
        Some(variant)
    }
}


/// A netlink socket. Wraps the underlying `libmnl` `mnl_socket` struct and provides a safe Rust
/// API.
///
/// Dropping an open socket will automatically try to close it. But any error during closing will
/// simply be discarded. So use the [`close`] method to catch and handle any close error.
///
/// [`close`]: #method.close
pub struct Socket {
    socket: *mut mnl_sys::mnl_socket,
}

impl Socket {
    /// Opens a new Netlink socket to the given bus ID, and binds it to group zero and with an
    /// automatic port id (MNL_SOCKET_AUTOPID).
    ///
    /// Use [`open`] and [`bind`] for more fine grained control.
    ///
    /// [`open`]: #method.open
    /// [`bind`]: #method.bind
    pub fn new(bus: Bus) -> io::Result<Self> {
        let socket = Self::open(bus)?;
        socket.bind(0, mnl_sys::MNL_SOCKET_AUTOPID)?;
        Ok(socket)
    }

    /// Opens a new Netlink socket to the given bus ID.
    pub fn open(bus: Bus) -> io::Result<Self> {
        Ok(Socket {
            socket: cvt(unsafe { mnl_sys::mnl_socket_open(bus as i32) })?,
        })
    }

    /// Bind the Netlink socket.
    pub fn bind(&self, groups: c_uint, pid: pid_t) -> io::Result<()> {
        cvt(unsafe { mnl_sys::mnl_socket_bind(self.socket, groups, pid) })?;
        Ok(())
    }

    /// Sends a Netlink message with the given slice of data. Returns the number of bytes sent if
    /// successful.
    pub fn send(&self, data: &[u8]) -> io::Result<usize> {
        let len = data.len();
        let ptr = data.as_ptr() as *const c_void;
        log::debug!("Sending {} byte netlink message", len);
        let result = cvt(unsafe { mnl_sys::mnl_socket_sendto(self.socket, ptr, len) })?;
        Ok(result as usize)
    }

    /// Sends all messages in an iterator to the socket. Aborts as soon as an error is encountered.
    /// Aborts and returns `Other` error if a send operation returned without sending the entire
    /// message.
    pub fn send_all<'a, I>(&self, iter: I) -> io::Result<()>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        for data in iter {
            if self.send(data)? < data.len() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "sendto did not send entire message",
                ));
            }
        }
        Ok(())
    }

    /// Receives a Netlink message from the socket. Returns the number of bytes written to `buffer`
    /// on success. If the message does not fit in the provided buffer an error will be returned
    /// a partial message will be written to `buffer` and the rest discarded.
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let len = buffer.len();
        let ptr = buffer.as_mut_ptr() as *mut c_void;
        let result = cvt(unsafe { mnl_sys::mnl_socket_recvfrom(self.socket, ptr, len) })?;
        Ok(result as usize)
    }

    /// Obtain Netlink PortID from netlink socket.
    pub fn portid(&self) -> c_uint {
        unsafe { mnl_sys::mnl_socket_get_portid(self.socket) }
    }

    /// Tries to close the socket, returns the corresponding error on failure.
    pub fn close(self) -> io::Result<()> {
        cvt(unsafe { mnl_sys::mnl_socket_close(self.socket) })?;
        mem::forget(self);
        Ok(())
    }

    /// Returns the pointer to the underlying C struct. Can be used with the `mnl_sys` crate to
    /// perform actions not yet exposed in this safe abstraction.
    pub fn as_raw_socket(&self) -> *mut mnl_sys::mnl_socket {
        self.socket
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        unsafe { mnl_sys::mnl_socket_close(self.socket) };
    }
}

impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> RawFd {
        unsafe { mnl_sys::mnl_socket_get_fd(self.socket) }
    }
}
