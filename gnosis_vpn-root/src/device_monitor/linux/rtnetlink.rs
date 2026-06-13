use futures::StreamExt;
use rtnetlink::{
    constants::{RTMGRP_IPV4_IFADDR, RTMGRP_IPV4_ROUTE, RTMGRP_IPV6_ROUTE, RTMGRP_LINK, RTMGRP_NOTIFY},
    packet_core::NetlinkPayload,
    packet_route::{RouteNetlinkMessage, address::AddressAttribute, link::LinkAttribute},
    sys::{AsyncSocket, SocketAddr},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::device_monitor::NetworkEvent;

pub fn start(tx: mpsc::Sender<NetworkEvent>) -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    let (mut conn, _handle, messages) = rtnetlink::new_connection()?;

    // Match Mullvad's subscription: route + link + notify.
    // RTMGRP_NOTIFY is required to receive events for routes tagged RTM_F_NOTIFY,
    // which systemd-networkd (used on NixOS) sets on its routes.
    let mgroup_flags = RTMGRP_LINK | RTMGRP_NOTIFY | RTMGRP_IPV4_IFADDR | RTMGRP_IPV4_ROUTE | RTMGRP_IPV6_ROUTE;
    let addr = SocketAddr::new(0, mgroup_flags);
    conn.socket_mut().socket_mut().bind(&addr)?;

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
                    if let NetlinkPayload::InnerMessage(inner) = msg.payload {
                        tracing::debug!(kind = %msg_kind(&inner), "device monitor: rtnetlink event");
                        if let Some(event) = to_network_event(inner) {
                            let _ = tx.try_send(event);
                        }
                    }
                }
            }
        }
    }
}

fn msg_kind(msg: &RouteNetlinkMessage) -> &'static str {
    match msg {
        RouteNetlinkMessage::NewLink(_) => "NewLink",
        RouteNetlinkMessage::DelLink(_) => "DelLink",
        RouteNetlinkMessage::NewAddress(_) => "NewAddress",
        RouteNetlinkMessage::DelAddress(_) => "DelAddress",
        RouteNetlinkMessage::NewRoute(_) => "NewRoute",
        RouteNetlinkMessage::DelRoute(_) => "DelRoute",
        _ => "other",
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
