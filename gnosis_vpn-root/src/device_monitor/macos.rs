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
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };

    let async_fd = match AsyncFd::new(unsafe { OwnedFd::from_raw_fd(fd) }) {
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
            let ifm = unsafe { &*(buf.as_ptr() as *const libc::if_msghdr) };
            let name = if_name(ifm.ifm_index as u32);
            let index = ifm.ifm_index as u32;
            Some(NetworkEvent::LinkChanged { index, name })
        }
        libc::RTM_NEWADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return None;
            }
            let ifam = unsafe { &*(buf.as_ptr() as *const libc::ifa_msghdr) };
            let name = if_name(ifam.ifam_index as u32);
            let index = ifam.ifam_index as u32;
            Some(NetworkEvent::AddressAdded { index, name })
        }
        libc::RTM_DELADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return None;
            }
            let ifam = unsafe { &*(buf.as_ptr() as *const libc::ifa_msghdr) };
            let name = if_name(ifam.ifam_index as u32);
            let index = ifam.ifam_index as u32;
            Some(NetworkEvent::AddressRemoved { index, name })
        }
        libc::RTM_IFINFO2 => {
            if buf.len() < std::mem::size_of::<libc::if_msghdr2>() {
                return None;
            }
            let ifm = unsafe { &*(buf.as_ptr() as *const libc::if_msghdr2) };
            let name = if_name(ifm.ifm_index as u32);
            let index = ifm.ifm_index as u32;
            Some(NetworkEvent::LinkChanged { index, name })
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

fn if_name(index: u32) -> String {
    let mut buf = [0i8; libc::IF_NAMESIZE];
    let ptr = unsafe { libc::if_indextoname(index, buf.as_mut_ptr()) };
    if ptr.is_null() {
        return format!("if#{index}");
    }
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
