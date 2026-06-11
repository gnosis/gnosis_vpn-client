use std::os::unix::io::{FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;
use tokio_util::sync::CancellationToken;

pub fn start_pf_route() -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run(owned_cancel));
    (cancel, handle)
}

async fn run(cancel: CancellationToken) {
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
                        Ok(Ok(n)) => log_event(&buf[..n]),
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

fn log_event(buf: &[u8]) {
    if buf.len() < std::mem::size_of::<libc::rt_msghdr>() {
        return;
    }
    let rtm_type = buf[3] as libc::c_int;
    match rtm_type {
        libc::RTM_IFINFO => {
            if buf.len() < std::mem::size_of::<libc::if_msghdr>() {
                return;
            }
            let ifm = unsafe { &*(buf.as_ptr() as *const libc::if_msghdr) };
            let name = if_name(ifm.ifm_index as u32);
            let index = ifm.ifm_index;
            tracing::info!(index, name, "network link changed");
        }
        libc::RTM_NEWADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return;
            }
            let ifam = unsafe { &*(buf.as_ptr() as *const libc::ifa_msghdr) };
            let name = if_name(ifam.ifam_index as u32);
            let index = ifam.ifam_index;
            tracing::info!(index, name, "network address added");
        }
        libc::RTM_DELADDR => {
            if buf.len() < std::mem::size_of::<libc::ifa_msghdr>() {
                return;
            }
            let ifam = unsafe { &*(buf.as_ptr() as *const libc::ifa_msghdr) };
            let name = if_name(ifam.ifam_index as u32);
            let index = ifam.ifam_index;
            tracing::info!(index, name, "network address removed");
        }
        libc::RTM_ADD => {
            let rtm = unsafe { &*(buf.as_ptr() as *const libc::rt_msghdr) };
            let name = if_name(rtm.rtm_index as u32);
            let index = rtm.rtm_index;
            tracing::info!(index, name, "route added");
        }
        libc::RTM_DELETE => {
            let rtm = unsafe { &*(buf.as_ptr() as *const libc::rt_msghdr) };
            let name = if_name(rtm.rtm_index as u32);
            let index = rtm.rtm_index;
            tracing::info!(index, name, "route removed");
        }
        libc::RTM_CHANGE => {
            let rtm = unsafe { &*(buf.as_ptr() as *const libc::rt_msghdr) };
            let name = if_name(rtm.rtm_index as u32);
            let index = rtm.rtm_index;
            tracing::info!(index, name, "route changed");
        }
        _ => {}
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
