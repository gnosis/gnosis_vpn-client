use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::NetworkEvent;

pub fn start_pf_route(tx: mpsc::Sender<NetworkEvent>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run(tx, owned_cancel));
    (cancel, handle)
}

async fn run(tx: mpsc::Sender<NetworkEvent>, cancel: CancellationToken) {
    let fd = unsafe { libc::socket(libc::AF_ROUTE, libc::SOCK_RAW, 0) };
    if fd < 0 {
        tracing::error!(
            error = ?std::io::Error::last_os_error(),
            "device monitor: failed to create PF_ROUTE socket"
        );
        return;
    }

    // Take ownership now so the fd is closed on all subsequent exit paths.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    if unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        tracing::error!(
            error = ?std::io::Error::last_os_error(),
            "device monitor: failed to set non-blocking mode on PF_ROUTE socket"
        );
        return;
    }

    let async_fd = match AsyncFd::new(owned) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = ?e, "device monitor: failed to register PF_ROUTE socket with tokio");
            return;
        }
    };

    tracing::debug!("device monitor started");
    let mut buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("device monitor stopping");
                return;
            }
            result = async_fd.readable() => {
                let mut guard = match result {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::error!(error = ?e, "device monitor: PF_ROUTE socket error");
                        return;
                    }
                };
                loop {
                    match guard.try_io(|fd| {
                        let n = unsafe {
                            libc::read(
                                std::os::unix::io::AsRawFd::as_raw_fd(fd.get_ref()),
                                buf.as_mut_ptr().cast(),
                                buf.len(),
                            )
                        };
                        if n < 0 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(n as usize)
                        }
                    }) {
                        Ok(Ok(n)) => {
                            if let Some(event) = to_network_event(&buf[..n]) {
                                // try_send keeps the drain loop non-blocking; if the channel
                                // is full we drop the event rather than stalling the socket reader.
                                let _ = tx.try_send(event);
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = ?e, "device monitor: PF_ROUTE read error");
                            return;
                        }
                        Err(_would_block) => break,
                    }
                }
            }
        }
    }
}

// RTM_IFANNOUNCE (0x11) fires when an interface is added or removed.
// The name is embedded in the message and is intact even during removal,
// unlike RTM_IFINFO where if_indextoname may fail if the interface is already gone.
//
// if_announcemsghdr layout (all fields native-endian):
//   [0..2]  ifan_msglen  u16
//   [2]     ifan_version u8
//   [3]     ifan_type    u8
//   [4..6]  ifan_index   u16
//   [6..22] ifan_name    [u8; IF_NAMESIZE=16], null-terminated
//   [22..24] ifan_what   u16  (0=arrival, 1=departure)
const RTM_IFANNOUNCE: libc::c_int = 0x11;
const IFAN_DEPARTURE: u16 = 1;
const IFAN_INDEX_OFFSET: usize = 4;
const IFAN_NAME_OFFSET: usize = 6;
const IFAN_NAME_LEN: usize = 16; // IF_NAMESIZE on macOS
const IFAN_WHAT_OFFSET: usize = 22;
const IFAN_MIN_LEN: usize = 24;

fn to_network_event(buf: &[u8]) -> Option<NetworkEvent> {
    if buf.len() < std::mem::size_of::<libc::rt_msghdr>() {
        return None;
    }
    let rtm_type = buf[3] as libc::c_int;
    match rtm_type {
        libc::RTM_IFINFO => {
            if buf.len() < std::mem::size_of::<libc::if_msghdr>() {
                return None;
            }
            let offset = std::mem::offset_of!(libc::if_msghdr, ifm_index);
            let index = u16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap()) as u32;
            match if_name(index) {
                // if_indextoname succeeded: interface still exists, flags changed
                Some(name) => Some(NetworkEvent::LinkChanged { index, name }),
                // if_indextoname failed: interface is gone, this is a deletion signal
                None => Some(NetworkEvent::LinkRemoved {
                    index,
                    name: format!("if#{index}"),
                }),
            }
        }
        libc::RTM_NEWADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return None;
            }
            let offset = std::mem::offset_of!(libc::ifa_msghdr, ifam_index);
            let index = u16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap()) as u32;
            let name = if_name(index).unwrap_or_else(|| format!("if#{index}"));
            Some(NetworkEvent::AddressAdded { index, name })
        }
        libc::RTM_DELADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return None;
            }
            let offset = std::mem::offset_of!(libc::ifa_msghdr, ifam_index);
            let index = u16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap()) as u32;
            let name = if_name(index).unwrap_or_else(|| format!("if#{index}"));
            Some(NetworkEvent::AddressRemoved { index, name })
        }
        libc::RTM_IFINFO2 => {
            if buf.len() < std::mem::size_of::<libc::if_msghdr2>() {
                return None;
            }
            let offset = std::mem::offset_of!(libc::if_msghdr2, ifm_index);
            let index = u16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap()) as u32;
            match if_name(index) {
                Some(name) => Some(NetworkEvent::LinkChanged { index, name }),
                None => Some(NetworkEvent::LinkRemoved {
                    index,
                    name: format!("if#{index}"),
                }),
            }
        }
        RTM_IFANNOUNCE => {
            if buf.len() < IFAN_MIN_LEN {
                return None;
            }
            let index = u16::from_ne_bytes([buf[IFAN_INDEX_OFFSET], buf[IFAN_INDEX_OFFSET + 1]]) as u32;
            let name_bytes = &buf[IFAN_NAME_OFFSET..IFAN_NAME_OFFSET + IFAN_NAME_LEN];
            let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(IFAN_NAME_LEN);
            let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();
            let ifan_what = u16::from_ne_bytes([buf[IFAN_WHAT_OFFSET], buf[IFAN_WHAT_OFFSET + 1]]);
            if ifan_what == IFAN_DEPARTURE {
                Some(NetworkEvent::LinkRemoved { index, name })
            } else {
                None // arrival: RTM_IFINFO already covers this
            }
        }
        libc::RTM_ADD => {
            let details = route_details(buf);
            tracing::info!(pid = details.pid, destination = ?details.destination, "route added");
            Some(NetworkEvent::RouteAdded)
        }
        libc::RTM_DELETE => {
            let details = route_details(buf);
            tracing::info!(pid = details.pid, destination = ?details.destination, "route removed");
            Some(NetworkEvent::RouteRemoved)
        }
        libc::RTM_CHANGE => {
            let details = route_details(buf);
            tracing::info!(pid = details.pid, destination = ?details.destination, "route changed");
            Some(NetworkEvent::RouteChanged)
        }
        _ => {
            tracing::debug!(rtm_type, "device monitor: unhandled RTM type");
            None
        }
    }
}

/// Origin and destination of an RTM_ADD/RTM_DELETE/RTM_CHANGE message, for
/// attributing route churn: `pid` is the process that made the change (0 for
/// the kernel itself), `destination` the affected route's RTA_DST address.
struct RouteDetails {
    pid: i32,
    destination: Option<std::net::IpAddr>,
}

fn route_details(buf: &[u8]) -> RouteDetails {
    // Caller guarantees the full rt_msghdr is present (to_network_event checks).
    let pid_offset = std::mem::offset_of!(libc::rt_msghdr, rtm_pid);
    let pid = i32::from_ne_bytes(buf[pid_offset..pid_offset + 4].try_into().unwrap());
    let addrs_offset = std::mem::offset_of!(libc::rt_msghdr, rtm_addrs);
    let addrs = i32::from_ne_bytes(buf[addrs_offset..addrs_offset + 4].try_into().unwrap());
    let destination = if addrs & libc::RTA_DST != 0 {
        // Sockaddrs follow the header in ascending RTA bit order, so RTA_DST is
        // always first when present: sa_len at +0, sa_family at +1, then the
        // family-specific address layout.
        parse_sockaddr(&buf[std::mem::size_of::<libc::rt_msghdr>()..])
    } else {
        None
    };
    RouteDetails { pid, destination }
}

fn parse_sockaddr(buf: &[u8]) -> Option<std::net::IpAddr> {
    let family = *buf.get(1)? as libc::c_int;
    match family {
        libc::AF_INET => {
            let octets: [u8; 4] = buf.get(4..8)?.try_into().ok()?;
            Some(std::net::IpAddr::V4(octets.into()))
        }
        libc::AF_INET6 => {
            let octets: [u8; 16] = buf.get(8..24)?.try_into().ok()?;
            Some(std::net::IpAddr::V6(octets.into()))
        }
        _ => None,
    }
}

fn if_name(index: u32) -> Option<String> {
    let mut buf = [0u8; libc::IF_NAMESIZE];
    let ptr = unsafe { libc::if_indextoname(index, buf.as_mut_ptr().cast()) };
    if ptr.is_null() {
        return None;
    }
    std::ffi::CStr::from_bytes_until_nul(&buf)
        .ok()
        .map(|s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn synthetic_route_msg(pid: i32, dst: Option<IpAddr>) -> Vec<u8> {
        let header_len = std::mem::size_of::<libc::rt_msghdr>();
        let mut buf = vec![0u8; header_len];
        buf[3] = libc::RTM_ADD as u8;
        let pid_offset = std::mem::offset_of!(libc::rt_msghdr, rtm_pid);
        buf[pid_offset..pid_offset + 4].copy_from_slice(&pid.to_ne_bytes());
        if let Some(dst) = dst {
            let addrs_offset = std::mem::offset_of!(libc::rt_msghdr, rtm_addrs);
            buf[addrs_offset..addrs_offset + 4].copy_from_slice(&libc::RTA_DST.to_ne_bytes());
            match dst {
                IpAddr::V4(v4) => {
                    let mut sockaddr = [0u8; 16];
                    sockaddr[0] = 16; // sin_len
                    sockaddr[1] = libc::AF_INET as u8;
                    sockaddr[4..8].copy_from_slice(&v4.octets());
                    buf.extend_from_slice(&sockaddr);
                }
                IpAddr::V6(v6) => {
                    let mut sockaddr = [0u8; 28];
                    sockaddr[0] = 28; // sin6_len
                    sockaddr[1] = libc::AF_INET6 as u8;
                    sockaddr[8..24].copy_from_slice(&v6.octets());
                    buf.extend_from_slice(&sockaddr);
                }
            }
        }
        buf
    }

    #[test]
    fn route_details_extracts_pid_and_ipv4_destination() {
        let dst = IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1));
        let details = route_details(&synthetic_route_msg(4711, Some(dst)));
        assert_eq!(details.pid, 4711);
        assert_eq!(details.destination, Some(dst));
    }

    #[test]
    fn route_details_extracts_ipv6_destination() {
        let dst = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let details = route_details(&synthetic_route_msg(0, Some(dst)));
        assert_eq!(details.pid, 0);
        assert_eq!(details.destination, Some(dst));
    }

    #[test]
    fn route_details_without_destination_sockaddr() {
        let details = route_details(&synthetic_route_msg(99, None));
        assert_eq!(details.pid, 99);
        assert_eq!(details.destination, None);
    }

    #[test]
    fn route_details_tolerates_a_truncated_sockaddr() {
        let mut buf = synthetic_route_msg(7, Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))));
        buf.truncate(std::mem::size_of::<libc::rt_msghdr>() + 3);
        let details = route_details(&buf);
        assert_eq!(details.pid, 7);
        assert_eq!(details.destination, None);
    }
}
