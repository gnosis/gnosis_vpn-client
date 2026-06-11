use futures::StreamExt;
use rtnetlink::{
    MulticastGroup,
    packet_core::NetlinkPayload,
    packet_route::{RouteNetlinkMessage, link::LinkAttribute},
};
use tokio_util::sync::CancellationToken;

pub fn start() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
    let (conn, _handle, messages) = rtnetlink::new_multicast_connection(&[MulticastGroup::Link])?;
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
                        log_link_event(inner);
                    }
                }
            }
        }
    }
}

fn log_link_event(msg: RouteNetlinkMessage) {
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
