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
use super::route_ops::{RouteOps, WanRoute};

/// Returns true if `prefix/len` covers `dest` (i.e. they share the same leading `len` bits).
fn covers(prefix: Ipv4Addr, len: u8, dest: Ipv4Addr) -> bool {
    if len == 0 {
        return true;
    }
    let mask = !0u32 << (32 - len);
    (u32::from(prefix) & mask) == (u32::from(dest) & mask)
}

/// Production [`RouteOps`] for Linux backed by an `rtnetlink::Handle`.
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

    async fn resolve_ifname(&self, index: u32) -> Result<String, Error> {
        let links: Vec<_> = self.handle.link().get().execute().try_collect().await?;
        links
            .iter()
            .find(|l| l.header.index == index)
            .and_then(|l| {
                l.attributes.iter().find_map(|a| match a {
                    LinkAttribute::IfName(n) => Some(n.clone()),
                    _ => None,
                })
            })
            .ok_or_else(|| Error::General(format!("interface name not found for index {index}")))
    }
}

#[async_trait]
impl RouteOps for NetlinkRouteOps {
    async fn get_wan_route_for(&self, dest: Ipv4Addr, exclude_iface: &str) -> Result<Option<WanRoute>, Error> {
        let exclude_idx = self.resolve_ifindex(exclude_iface).await.ok();

        let routes: Vec<_> = self
            .handle
            .route()
            .get(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().build())
            .execute()
            .try_collect()
            .await?;

        // Among all main-table routes covering `dest` and not via the VPN tunnel,
        // pick the most specific (highest prefix length).
        let best = routes
            .iter()
            .filter(|r| r.header.table == 254)
            .filter(|r| {
                let oif = r.attributes.iter().find_map(|a| match a {
                    RouteAttribute::Oif(idx) => Some(*idx),
                    _ => None,
                });
                exclude_idx.is_none() || oif != exclude_idx
            })
            .filter(|r| {
                let prefix_len = r.header.destination_prefix_length;
                let prefix_addr = r
                    .attributes
                    .iter()
                    .find_map(|a| match a {
                        RouteAttribute::Destination(RouteAddress::Inet(ip)) => Some(*ip),
                        _ => None,
                    })
                    .unwrap_or(Ipv4Addr::UNSPECIFIED);
                covers(prefix_addr, prefix_len, dest)
            })
            .filter(|r| r.attributes.iter().any(|a| matches!(a, RouteAttribute::Oif(_))))
            .max_by_key(|r| {
                let metric = r
                    .attributes
                    .iter()
                    .find_map(|a| match a {
                        RouteAttribute::Priority(m) => Some(*m),
                        _ => None,
                    })
                    .unwrap_or(0);
                (r.header.destination_prefix_length, std::cmp::Reverse(metric))
            });

        let Some(route) = best else {
            return Ok(None);
        };

        let oif = route
            .attributes
            .iter()
            .find_map(|a| match a {
                RouteAttribute::Oif(idx) => Some(*idx),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        let device = self.resolve_ifname(oif).await?;

        let gateway = route.attributes.iter().find_map(|a| match a {
            RouteAttribute::Gateway(RouteAddress::Inet(ip)) => Some(ip.to_string()),
            _ => None,
        });

        let src_ip = route.attributes.iter().find_map(|a| match a {
            RouteAttribute::PrefSource(RouteAddress::Inet(ip)) => Some(*ip),
            _ => None,
        });

        Ok(Some(WanRoute {
            device,
            gateway,
            src_ip,
        }))
    }

    async fn get_route_via_device(&self, dest: Ipv4Addr, device: &str) -> Result<Option<WanRoute>, Error> {
        // If the interface is gone entirely, treat that as no route.
        let device_idx = match self.resolve_ifindex(device).await {
            Ok(idx) => idx,
            Err(_) => return Ok(None),
        };

        let routes: Vec<_> = self
            .handle
            .route()
            .get(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().build())
            .execute()
            .try_collect()
            .await?;

        let best = routes
            .iter()
            .filter(|r| r.header.table == 254)
            .filter(|r| {
                r.attributes
                    .iter()
                    .any(|a| matches!(a, RouteAttribute::Oif(idx) if *idx == device_idx))
            })
            .filter(|r| {
                let prefix_len = r.header.destination_prefix_length;
                let prefix_addr = r
                    .attributes
                    .iter()
                    .find_map(|a| match a {
                        RouteAttribute::Destination(RouteAddress::Inet(ip)) => Some(*ip),
                        _ => None,
                    })
                    .unwrap_or(Ipv4Addr::UNSPECIFIED);
                covers(prefix_addr, prefix_len, dest)
            })
            .max_by_key(|r| {
                let metric = r
                    .attributes
                    .iter()
                    .find_map(|a| match a {
                        RouteAttribute::Priority(m) => Some(*m),
                        _ => None,
                    })
                    .unwrap_or(0);
                (r.header.destination_prefix_length, std::cmp::Reverse(metric))
            });

        let Some(route) = best else {
            return Ok(None);
        };

        let gateway = route.attributes.iter().find_map(|a| match a {
            RouteAttribute::Gateway(RouteAddress::Inet(ip)) => Some(ip.to_string()),
            _ => None,
        });

        let src_ip = route.attributes.iter().find_map(|a| match a {
            RouteAttribute::PrefSource(RouteAddress::Inet(ip)) => Some(*ip),
            _ => None,
        });

        Ok(Some(WanRoute {
            device: device.to_owned(),
            gateway,
            src_ip,
        }))
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── covers ───────────────────────────────────────────────────────────────

    #[test]
    fn covers_default_route_matches_any_ip() {
        assert!(covers(Ipv4Addr::new(0, 0, 0, 0), 0, Ipv4Addr::new(1, 1, 1, 1)));
        assert!(covers(Ipv4Addr::new(0, 0, 0, 0), 0, Ipv4Addr::new(192, 168, 0, 1)));
    }

    #[test]
    fn covers_subnet_matches_contained_host() {
        assert!(covers(
            Ipv4Addr::new(192, 168, 1, 0),
            24,
            Ipv4Addr::new(192, 168, 1, 100)
        ));
    }

    #[test]
    fn covers_subnet_rejects_outside_host() {
        assert!(!covers(
            Ipv4Addr::new(192, 168, 1, 0),
            24,
            Ipv4Addr::new(192, 168, 2, 1)
        ));
    }

    #[test]
    fn covers_host_route_exact_match_only() {
        let ip = Ipv4Addr::new(10, 0, 0, 1);
        assert!(covers(ip, 32, ip));
        assert!(!covers(ip, 32, Ipv4Addr::new(10, 0, 0, 2)));
    }

    #[test]
    fn covers_vpn_split_halves() {
        // 0.0.0.0/1 covers the lower half of IPv4 space
        assert!(covers(Ipv4Addr::new(0, 0, 0, 0), 1, Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!covers(Ipv4Addr::new(0, 0, 0, 0), 1, Ipv4Addr::new(128, 0, 0, 1)));
        // 128.0.0.0/1 covers the upper half
        assert!(covers(Ipv4Addr::new(128, 0, 0, 0), 1, Ipv4Addr::new(200, 0, 0, 1)));
    }

    // ── parse_dest ───────────────────────────────────────────────────────────

    #[test]
    fn parse_dest_cidr_notation() {
        let (addr, prefix) = NetlinkRouteOps::parse_dest("10.0.0.0/8").unwrap();
        assert_eq!(addr, Ipv4Addr::new(10, 0, 0, 0));
        assert_eq!(prefix, 8);
    }

    #[test]
    fn parse_dest_host_address_defaults_to_slash32() {
        let (addr, prefix) = NetlinkRouteOps::parse_dest("1.2.3.4").unwrap();
        assert_eq!(addr, Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(prefix, 32);
    }

    #[test]
    fn parse_dest_rejects_invalid_address() {
        assert!(NetlinkRouteOps::parse_dest("not-an-ip").is_err());
        assert!(NetlinkRouteOps::parse_dest("1.2.3.4/256").is_err()); // 256 overflows u8
    }
}
