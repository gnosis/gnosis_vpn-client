//! Linux route operations using rtnetlink.
//!
//! [`NetlinkRouteOps`] implements [`RouteOps`] by converting string-based
//! route operations into typed netlink messages via `rtnetlink::Handle`.

use async_trait::async_trait;
use futures::TryStreamExt;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::{RouteAddress, RouteAttribute};
use std::net::Ipv4Addr;
use std::str::FromStr;

use super::Error;
use super::route_ops::RouteOps;

/// Production [`RouteOps`] for Linux backed by an `rtnetlink::Handle`.
///
/// Reuses the same netlink connection as [`RealNetlinkOps`](super::netlink_ops::RealNetlinkOps)
/// since `rtnetlink::Handle` is cheaply cloneable.
#[derive(Clone)]
pub struct NetlinkRouteOps {
    handle: rtnetlink::Handle,
}

impl NetlinkRouteOps {
    pub fn new(handle: rtnetlink::Handle) -> Self {
        Self { handle }
    }

    /// Parse a destination string like "10.0.0.0/8" or "1.2.3.4" into (addr, prefix_len).
    fn parse_dest(dest: &str) -> Result<(Ipv4Addr, u8), Error> {
        if let Some((addr_str, prefix_str)) = dest.split_once('/') {
            let addr = Ipv4Addr::from_str(addr_str)
                .map_err(|e| Error::General(format!("invalid route destination address: {e}")))?;
            let prefix_len: u8 = prefix_str
                .parse()
                .map_err(|e| Error::General(format!("invalid route prefix length: {e}")))?;
            Ok((addr, prefix_len))
        } else {
            let addr =
                Ipv4Addr::from_str(dest).map_err(|e| Error::General(format!("invalid route destination: {e}")))?;
            // Host route
            Ok((addr, 32))
        }
    }

    /// Resolve a device name to its interface index.
    async fn resolve_ifindex(&self, device: &str) -> Result<u32, Error> {
        let links: Vec<_> = self
            .handle
            .link()
            .get()
            .match_name(device.to_string())
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::General(format!("failed to resolve interface '{device}': {e}")))?;

        links
            .first()
            .map(|l| l.header.index)
            .ok_or_else(|| Error::General(format!("interface '{device}' not found")))
    }
}

#[async_trait]
impl RouteOps for NetlinkRouteOps {
    async fn get_default_interface(&self) -> Result<(String, Option<String>), Error> {
        // List all IPv4 routes and find the default (0.0.0.0/0)
        let routes: Vec<_> = self
            .handle
            .route()
            .get(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().build())
            .execute()
            .try_collect()
            .await?;

        // Find the default route (prefix_len == 0)
        let default_route = routes
            .iter()
            .filter(|r| r.header.destination_prefix_length == 0)
            .min_by_key(|r| {
                // Prefer routes with lower metric
                r.attributes
                    .iter()
                    .find_map(|a| match a {
                        RouteAttribute::Priority(p) => Some(*p),
                        _ => None,
                    })
                    .unwrap_or(0)
            })
            .ok_or(Error::NoInterface)?;

        // Extract interface index
        let if_index = default_route
            .attributes
            .iter()
            .find_map(|a| match a {
                RouteAttribute::Oif(idx) => Some(*idx),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        // Extract gateway
        let gateway = default_route.attributes.iter().find_map(|a| match a {
            RouteAttribute::Gateway(RouteAddress::Inet(ip)) => Some(ip.to_string()),
            _ => None,
        });

        // Resolve ifindex to name
        let links: Vec<_> = self.handle.link().get().execute().try_collect().await?;

        let if_name = links
            .iter()
            .find(|l| l.header.index == if_index)
            .and_then(|l| {
                l.attributes.iter().find_map(|a| match a {
                    LinkAttribute::IfName(n) => Some(n.clone()),
                    _ => None,
                })
            })
            .ok_or_else(|| Error::General(format!("interface name not found for index {if_index}")))?;

        Ok((if_name, gateway))
    }

    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error> {
        let (addr, prefix_len) = Self::parse_dest(dest)?;
        let if_index = self.resolve_ifindex(device).await?;

        let mut builder = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(addr, prefix_len)
            .output_interface(if_index);

        if let Some(gw_str) = gateway {
            let gw = Ipv4Addr::from_str(gw_str).map_err(|e| Error::General(format!("invalid gateway address: {e}")))?;
            builder = builder.gateway(gw);
        }

        self.handle.route().add(builder.build()).execute().await?;
        Ok(())
    }

    async fn route_del(&self, dest: &str, device: &str) -> Result<(), Error> {
        let (addr, prefix_len) = Self::parse_dest(dest)?;
        let if_index = self.resolve_ifindex(device).await?;

        let msg = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(addr, prefix_len)
            .output_interface(if_index)
            .build();

        self.handle.route().del(msg).execute().await?;
        Ok(())
    }

    async fn flush_routing_cache(&self) -> Result<(), Error> {
        // The routing cache was removed in Linux 3.6.
        // `ip route flush cache` is a no-op on modern kernels.
        Ok(())
    }
}
