use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub async fn start() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    #[cfg(target_os = "linux")]
    {
        if probe_rtnetlink_multicast().await {
            tracing::info!("device monitor: using rtnetlink");
            return linux::start_rtnetlink();
        }
        tracing::warn!("device monitor: rtnetlink multicast not working, falling back to ip monitor subprocess");
        return Ok(subprocess::start("ip", &["monitor", "link", "address", "route"]));
    }

    #[cfg(target_os = "macos")]
    return Ok(macos::start_pf_route());

    #[allow(unreachable_code)]
    Err(std::io::Error::other("device monitor: unsupported platform"))
}

// ── Linux ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use futures::StreamExt;
    use rtnetlink::{
        MulticastGroup,
        packet_core::NetlinkPayload,
        packet_route::{
            RouteNetlinkMessage,
            address::AddressAttribute,
            link::LinkAttribute,
            route::{RouteAddress, RouteAttribute},
        },
    };
    use tokio_util::sync::CancellationToken;

    pub fn start_rtnetlink() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
        let (conn, _handle, messages) = rtnetlink::new_multicast_connection(&[
            MulticastGroup::Link,
            MulticastGroup::Ipv4Ifaddr,
            MulticastGroup::Ipv4Route,
        ])?;
        tokio::spawn(conn);
        let cancel = CancellationToken::new();
        let owned_cancel = cancel.clone();
        let handle = tokio::spawn(run(messages, owned_cancel));
        Ok((cancel, handle))
    }

    async fn run(
        mut messages: futures::channel::mpsc::UnboundedReceiver<(
            rtnetlink::packet_core::NetlinkMessage<RouteNetlinkMessage>,
            rtnetlink::sys::SocketAddr,
        )>,
        cancel: CancellationToken,
    ) {
        tracing::debug!("device monitor started");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("device monitor stopping");
                    return;
                }
                msg = messages.next() => match msg {
                    None => {
                        tracing::warn!("device monitor netlink channel closed unexpectedly");
                        return;
                    }
                    Some((msg, _)) => {
                        if let NetlinkPayload::InnerMessage(inner) = msg.payload {
                            log_event(inner);
                        }
                    }
                }
            }
        }
    }

    fn log_event(msg: RouteNetlinkMessage) {
        match msg {
            RouteNetlinkMessage::NewLink(link) => {
                let name = link_name(&link.attributes);
                let index = link.header.index;
                let flags = link.header.flags;
                tracing::info!(index, name, ?flags, "network link changed");
            }
            RouteNetlinkMessage::DelLink(link) => {
                let name = link_name(&link.attributes);
                let index = link.header.index;
                tracing::info!(index, name, "network link removed");
            }
            RouteNetlinkMessage::NewAddress(addr) => {
                let ip = addr_ip(&addr.attributes);
                let index = addr.header.index;
                let prefix_len = addr.header.prefix_len;
                tracing::info!(index, %ip, prefix_len, "network address added");
            }
            RouteNetlinkMessage::DelAddress(addr) => {
                let ip = addr_ip(&addr.attributes);
                let index = addr.header.index;
                let prefix_len = addr.header.prefix_len;
                tracing::info!(index, %ip, prefix_len, "network address removed");
            }
            RouteNetlinkMessage::NewRoute(route) => {
                let dst = route_dst(&route.attributes);
                let gw = route_gw(&route.attributes);
                let prefix_len = route.header.destination_prefix_length;
                tracing::info!(dst, gw, prefix_len, "route added");
            }
            RouteNetlinkMessage::DelRoute(route) => {
                let dst = route_dst(&route.attributes);
                let gw = route_gw(&route.attributes);
                let prefix_len = route.header.destination_prefix_length;
                tracing::info!(dst, gw, prefix_len, "route removed");
            }
            _ => {}
        }
    }

    fn link_name(attrs: &[LinkAttribute]) -> &str {
        attrs
            .iter()
            .find_map(|attr| {
                if let LinkAttribute::IfName(n) = attr {
                    Some(n.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("unknown")
    }

    fn addr_ip(attrs: &[AddressAttribute]) -> std::net::IpAddr {
        attrs
            .iter()
            .find_map(|attr| {
                if let AddressAttribute::Address(ip) = attr {
                    Some(*ip)
                } else {
                    None
                }
            })
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
    }

    fn fmt_route_addr(addr: &RouteAddress) -> String {
        match addr {
            RouteAddress::Inet(v) => v.to_string(),
            RouteAddress::Inet6(v) => v.to_string(),
            RouteAddress::Mpls(v) => format!("{v:?}"),
            _ => "unknown".to_string(),
        }
    }

    fn route_dst(attrs: &[RouteAttribute]) -> String {
        attrs
            .iter()
            .find_map(|attr| {
                if let RouteAttribute::Destination(addr) = attr {
                    Some(fmt_route_addr(addr))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "default".to_string())
    }

    fn route_gw(attrs: &[RouteAttribute]) -> String {
        attrs
            .iter()
            .find_map(|attr| {
                if let RouteAttribute::Gateway(addr) = attr {
                    Some(fmt_route_addr(addr))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "none".to_string())
    }
}

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
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
}

// ── subprocess fallback (Linux) ───────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod subprocess {
    use tokio::io::AsyncBufReadExt;
    use tokio_util::sync::CancellationToken;

    pub fn start(cmd: &'static str, args: &'static [&'static str]) -> (CancellationToken, tokio::task::JoinHandle<()>) {
        let cancel = CancellationToken::new();
        let owned_cancel = cancel.clone();
        let handle = tokio::spawn(run(cmd, args, owned_cancel));
        (cancel, handle)
    }

    async fn run(cmd: &str, args: &[&str], cancel: CancellationToken) {
        tracing::debug!("device monitor started (subprocess fallback)");
        let child = tokio::process::Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = ?e, "device monitor: failed to spawn subprocess");
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let mut lines = tokio::io::BufReader::new(stdout).lines();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    return;
                }
                line = lines.next_line() => match line {
                    Err(e) => {
                        tracing::error!(error = ?e, "device monitor: subprocess read error");
                        return;
                    }
                    Ok(None) => {
                        tracing::warn!("device monitor: subprocess exited unexpectedly");
                        return;
                    }
                    Ok(Some(line)) => {
                        if !line.is_empty() && !line.starts_with(char::is_whitespace) {
                            tracing::info!(event = line, "network change detected");
                        }
                    }
                }
            }
        }
    }
}

// ── Linux probe ───────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
use std::net::{IpAddr, Ipv4Addr};

#[cfg(target_os = "linux")]
const PROBE_ADDR: Ipv4Addr = Ipv4Addr::new(169, 254, 200, 200);

#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Verifies that rtnetlink multicast delivery actually works on this system.
///
/// Adds a temporary address to loopback (which the kernel broadcasts as a
/// NewAddress multicast event), then checks whether the event arrives on the
/// subscription channel within the probe timeout. Cleans up unconditionally.
#[cfg(target_os = "linux")]
async fn probe_rtnetlink_multicast() -> bool {
    use futures::StreamExt;
    use rtnetlink::MulticastGroup;

    let (conn, handle, mut messages) = match rtnetlink::new_multicast_connection(&[MulticastGroup::Ipv4Ifaddr]) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = ?e, "device monitor probe: failed to create netlink connection");
            return false;
        }
    };
    let conn_task = tokio::spawn(conn);

    let added = handle
        .address()
        .add(1u32, IpAddr::V4(PROBE_ADDR), 32)
        .execute()
        .await
        .is_ok();

    if !added {
        tracing::debug!("device monitor probe: failed to add probe address to loopback");
        conn_task.abort();
        return false;
    }

    let received = tokio::time::timeout(PROBE_TIMEOUT, messages.next())
        .await
        .is_ok_and(|msg| msg.is_some());

    let _ = tokio::process::Command::new("ip")
        .args(["addr", "del", "169.254.200.200/32", "dev", "lo"])
        .output()
        .await;

    conn_task.abort();
    received
}
