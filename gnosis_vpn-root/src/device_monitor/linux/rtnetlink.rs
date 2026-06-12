use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use futures::StreamExt;
use rtnetlink::{
    MulticastGroup,
    packet_core::NetlinkPayload,
    packet_route::{RouteNetlinkMessage, address::AddressAttribute, link::LinkAttribute},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::device_monitor::NetworkEvent;

const PROBE_ADDR: Ipv4Addr = Ipv4Addr::new(169, 254, 200, 200);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Verifies that rtnetlink multicast delivery actually works on this system.
///
/// Adds a temporary address to loopback (which the kernel broadcasts as a
/// NewAddress multicast event), then checks whether the event arrives on the
/// subscription channel within the probe timeout. Cleans up unconditionally.
pub async fn probe_multicast() -> bool {
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

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    let (conn, _handle, messages) = rtnetlink::new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv4Route,
    ])?;
    tokio::spawn(conn);
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run(messages, tx, cancel.clone()));
    Ok((cancel, handle))
}

async fn run(
    mut messages: futures::channel::mpsc::UnboundedReceiver<(
        rtnetlink::packet_core::NetlinkMessage<RouteNetlinkMessage>,
        rtnetlink::sys::SocketAddr,
    )>,
    tx: mpsc::Sender<NetworkEvent>,
    cancel: CancellationToken,
) {
    tracing::debug!("device monitor started (rtnetlink)");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("device monitor stopping");
                return;
            }
            msg = messages.next() => match msg {
                None => {
                    tracing::warn!("device monitor: netlink channel closed unexpectedly");
                    return;
                }
                Some((msg, _)) => {
                    if let NetlinkPayload::InnerMessage(inner) = msg.payload
                        && let Some(event) = to_network_event(inner)
                    {
                        let _ = tx.try_send(event);
                    }
                }
            }
        }
    }
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
            let name = addr_label(&addr.attributes, index);
            Some(NetworkEvent::AddressAdded { index, name })
        }
        RouteNetlinkMessage::DelAddress(addr) => {
            let index = addr.header.index;
            let name = addr_label(&addr.attributes, index);
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

fn addr_label(attrs: &[AddressAttribute], index: u32) -> String {
    attrs
        .iter()
        .find_map(|attr| {
            if let AddressAttribute::Label(n) = attr {
                Some(n.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| format!("if#{index}"))
}
