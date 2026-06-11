use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

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
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

// A link-local address on loopback that is safe to add/remove transiently.
const PROBE_ADDR: Ipv4Addr = Ipv4Addr::new(169, 254, 200, 200);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

pub async fn start() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    if probe_rtnetlink_multicast().await {
        tracing::info!("device monitor: using rtnetlink");
        start_rtnetlink()
    } else {
        tracing::warn!("device monitor: rtnetlink multicast not working, falling back to ip monitor subprocess");
        Ok(start_subprocess())
    }
}

/// Verifies that rtnetlink multicast delivery actually works on this system.
///
/// Adds a temporary address to loopback (which the kernel broadcasts as a
/// NewAddress multicast event), then checks whether the event arrives on the
/// subscription channel within the probe timeout.  Cleans up the address
/// unconditionally before returning.
async fn probe_rtnetlink_multicast() -> bool {
    let (conn, handle, mut messages) = match rtnetlink::new_multicast_connection(&[MulticastGroup::Ipv4Ifaddr]) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = ?e, "device monitor probe: failed to create netlink connection");
            return false;
        }
    };
    let conn_task = tokio::spawn(conn);

    let lo_index = 1u32;
    let added = handle
        .address()
        .add(lo_index, IpAddr::V4(PROBE_ADDR), 32)
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

    // Remove the probe address. Use the ip CLI since that's already a known-good path.
    let _ = tokio::process::Command::new("ip")
        .args(["addr", "del", "169.254.200.200/32", "dev", "lo"])
        .output()
        .await;

    conn_task.abort();
    received
}

fn start_rtnetlink() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    let (conn, _handle, messages) = rtnetlink::new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv4Route,
    ])?;
    tokio::spawn(conn);
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run_rtnetlink(messages, owned_cancel));
    Ok((cancel, handle))
}

fn start_subprocess() -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run_subprocess(owned_cancel));
    (cancel, handle)
}

async fn run_rtnetlink(
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

async fn run_subprocess(cancel: CancellationToken) {
    tracing::debug!("device monitor started (subprocess fallback)");
    let child = tokio::process::Command::new("ip")
        .args(["monitor", "link", "address", "route"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "device monitor: failed to spawn ip monitor subprocess");
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
                    tracing::error!(error = ?e, "device monitor: error reading ip monitor output");
                    return;
                }
                Ok(None) => {
                    tracing::warn!("device monitor: ip monitor subprocess exited unexpectedly");
                    return;
                }
                Ok(Some(line)) => {
                    // Skip blank lines and continuation lines (hardware address, etc.)
                    if !line.is_empty() && !line.starts_with(char::is_whitespace) {
                        tracing::info!(event = line, "network change detected");
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
