use std::os::unix::io::{FromRawFd, OwnedFd};

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
        libc::RTM_ADD => Some(NetworkEvent::RouteAdded),
        libc::RTM_DELETE => Some(NetworkEvent::RouteRemoved),
        libc::RTM_CHANGE => Some(NetworkEvent::RouteChanged),
        _ => {
            tracing::debug!(rtm_type, "device monitor: unhandled RTM type");
            None
        }
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
