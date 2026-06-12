//! Linux route operations using rtnetlink.
//!
//! [`NetlinkRouteOps`] implements [`RouteOps`] using typed netlink messages via `rtnetlink::Handle`.

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
/// Reuses the same netlink connection as [`RealNetlinkOps`](super::netlink_ops::RealNetlinkOps).
#[derive(Clone)]
pub struct NetlinkRouteOps {
    handle: rtnetlink::Handle,
}

impl NetlinkRouteOps {
    pub fn new(handle: rtnetlink::Handle) -> Self {
        Self { handle }
    }

    /// Parse a destination string like "10.0.0.0/8" or "1.2.3.4" into (addr, prefix_len).
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

        // Find the default route (prefix_len == 0) in the main routing table only.
        // wg-quick creates its own routing table with a 0/0 route at metric 0; including
        // other tables here would cause it to shadow the real WAN default route.
        let default_route = routes
            .iter()
            .filter(|r| r.header.destination_prefix_length == 0)
            .filter(|r| r.header.table == 254)
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

    async fn has_default_route(&self, device: &str, gateway: Option<&str>) -> Result<bool, Error> {
        let if_index = match self.resolve_ifindex(device).await {
            Ok(idx) => idx,
            Err(_) => return Ok(false),
        };

        let routes: Vec<_> = self
            .handle
            .route()
            .get(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().build())
            .execute()
            .try_collect()
            .await?;

        for route in &routes {
            let is_default = route.header.destination_prefix_length == 0;
            let is_main_table = route.header.table == 254;
            if !is_default || !is_main_table {
                continue;
            }

            let route_ifindex = route
                .attributes
                .iter()
                .find_map(|a| if let RouteAttribute::Oif(idx) = a { Some(*idx) } else { None });

            if route_ifindex != Some(if_index) {
                continue;
            }

            let route_gw = route.attributes.iter().find_map(|a| match a {
                RouteAttribute::Gateway(RouteAddress::Inet(ip)) => Some(ip.to_string()),
                _ => None,
            });

            if route_gw.as_deref() == gateway {
                return Ok(true);
            }
        }

        Ok(false)
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
}
