use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use futures::StreamExt;
use rtnetlink::{
    MulticastGroup,
    packet_core::NetlinkPayload,
    packet_route::{RouteNetlinkMessage, link::LinkAttribute},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::NetworkEvent;

const PROBE_ADDR: Ipv4Addr = Ipv4Addr::new(169, 254, 200, 200);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Verifies that rtnetlink multicast delivery actually works on this system.
///
/// Adds a temporary address to loopback (which the kernel broadcasts as a
/// NewAddress multicast event), then checks whether the event arrives on the
/// subscription channel within the probe timeout. Cleans up unconditionally.
pub async fn probe_rtnetlink_multicast() -> bool {
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

pub fn start_rtnetlink(
    tx: mpsc::Sender<NetworkEvent>,
) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    let (conn, _handle, messages) = rtnetlink::new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv4Route,
    ])?;
    tokio::spawn(conn);
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run_rtnetlink(messages, tx, owned_cancel));
    Ok((cancel, handle))
}

pub fn start_subprocess(tx: mpsc::Sender<NetworkEvent>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let handle = tokio::spawn(run_subprocess(tx, owned_cancel));
    (cancel, handle)
}

async fn run_rtnetlink(
    mut messages: futures::channel::mpsc::UnboundedReceiver<(
        rtnetlink::packet_core::NetlinkMessage<RouteNetlinkMessage>,
        rtnetlink::sys::SocketAddr,
    )>,
    tx: mpsc::Sender<NetworkEvent>,
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
                    if let NetlinkPayload::InnerMessage(inner) = msg.payload
                        && let Some(event) = to_network_event(inner) {
                            let _ = tx.try_send(event);
                        }
                }
            }
        }
    }
}

async fn run_subprocess(tx: mpsc::Sender<NetworkEvent>, cancel: CancellationToken) {
    use tokio::io::AsyncBufReadExt;

    tracing::debug!("device monitor started (subprocess fallback)");
    let child = tokio::process::Command::new("ip")
        .args(["monitor", "link", "address", "route"])
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
                    tracing::debug!(line, "device monitor: ip monitor line");
                    if !line.is_empty() && !line.starts_with(char::is_whitespace) {
                        let event = parse_ip_monitor_line(&line);
                        if tx.try_send(event).is_err() {
                            tracing::warn!(line, "device monitor: event dropped (channel full)");
                        }
                    }
                }
            }
        }
    }
}

// Parses a non-indented line from `ip monitor link address route`.
//
// Link lines have the form "<index>: <name>: <flags>..." where the name
// contains no whitespace. Everything else (routes, addresses) becomes
// RouteChanged, which is sufficient for the WAN-change check.
fn parse_ip_monitor_line(line: &str) -> NetworkEvent {
    let (deleted, rest) = match line.strip_prefix("Deleted ") {
        Some(r) => (true, r),
        None => (false, line),
    };

    // Try to parse as a link line: "<index>: <name>: ..."
    if let Some((idx_str, after_idx)) = rest.split_once(": ")
        && let Ok(index) = idx_str.parse::<u32>()
        && let Some((name, _)) = after_idx.split_once(": ")
    {
        // Interface names never contain whitespace
        if !name.contains(char::is_whitespace) {
            tracing::debug!(deleted, index, name, "device monitor: parsed as link event");
            return if deleted {
                NetworkEvent::LinkRemoved {
                    index,
                    name: name.to_owned(),
                }
            } else {
                NetworkEvent::LinkChanged {
                    index,
                    name: name.to_owned(),
                }
            };
        }
    }

    tracing::debug!(line, "device monitor: parsed as route/addr event");
    NetworkEvent::RouteChanged
}

fn to_network_event(msg: RouteNetlinkMessage) -> Option<NetworkEvent> {
    match msg {
        RouteNetlinkMessage::NewLink(link) => {
            let name = link_name(&link.attributes).to_owned();
            let index = link.header.index;
            Some(NetworkEvent::LinkChanged { index, name })
        }
        RouteNetlinkMessage::DelLink(link) => {
            let name = link_name(&link.attributes).to_owned();
            let index = link.header.index;
            Some(NetworkEvent::LinkRemoved { index, name })
        }
        RouteNetlinkMessage::NewAddress(addr) => {
            let index = addr.header.index;
            let name = format!("if#{index}");
            Some(NetworkEvent::AddressAdded { index, name })
        }
        RouteNetlinkMessage::DelAddress(addr) => {
            let index = addr.header.index;
            let name = format!("if#{index}");
            Some(NetworkEvent::AddressRemoved { index, name })
        }
        RouteNetlinkMessage::NewRoute(_) => Some(NetworkEvent::RouteAdded),
        RouteNetlinkMessage::DelRoute(_) => Some(NetworkEvent::RouteRemoved),
        _ => None,
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
