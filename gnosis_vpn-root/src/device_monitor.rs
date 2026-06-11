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

pub fn start() -> std::io::Result<(CancellationToken, tokio::task::JoinHandle<()>)> {
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
