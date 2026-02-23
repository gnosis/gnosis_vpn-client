//! Abstraction over rtnetlink operations for testability.
//!
//! Defines [`NetlinkOps`] trait and domain types ([`RouteSpec`], [`RuleSpec`], etc.)
//! that decouple routing logic from the raw netlink wire format.
//!
//! Production code uses [`RealNetlinkOps`] which wraps `rtnetlink::Handle`.
//! Tests use stateful mocks (see `mocks` module).

use async_trait::async_trait;
use futures::TryStreamExt;
use rtnetlink::packet_route::address::AddressAttribute;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::{RouteAddress, RouteAttribute};
use rtnetlink::packet_route::rule::RuleAttribute;

use std::net::{IpAddr, Ipv4Addr};

use super::Error;

// ============================================================================
// Domain Types
// ============================================================================

/// Route specification decoupled from rtnetlink wire format.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSpec {
    pub destination: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Option<Ipv4Addr>,
    pub if_index: u32,
    pub table_id: Option<u32>,
}

/// Policy routing rule specification.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleSpec {
    pub fw_mark: u32,
    pub table_id: u32,
    pub priority: u32,
}

/// Network link (interface) information.
#[derive(Debug, Clone)]
pub struct LinkInfo {
    pub index: u32,
    pub name: String,
}

/// IPv4 address assigned to an interface.
#[derive(Debug, Clone)]
pub struct AddrInfo {
    pub if_index: u32,
    pub addr: Ipv4Addr,
}

// ============================================================================
// Trait
// ============================================================================

/// Abstraction over netlink route/rule/link/address operations.
///
/// Implementors must be cheaply cloneable (e.g. via `Arc` or because the
/// underlying handle is already reference-counted).
#[async_trait]
pub trait NetlinkOps: Send + Sync + Clone {
    async fn route_add(&self, route: &RouteSpec) -> Result<(), Error>;
    async fn route_replace(&self, route: &RouteSpec) -> Result<(), Error>;
    async fn route_del(&self, route: &RouteSpec) -> Result<(), Error>;
    /// List routes, optionally filtered by table ID.
    /// `None` returns all IPv4 routes.
    async fn route_list(&self, table_id: Option<u32>) -> Result<Vec<RouteSpec>, Error>;

    async fn rule_add(&self, rule: &RuleSpec) -> Result<(), Error>;
    async fn rule_del(&self, rule: &RuleSpec) -> Result<(), Error>;
    async fn rule_list_v4(&self) -> Result<Vec<RuleSpec>, Error>;

    async fn link_list(&self) -> Result<Vec<LinkInfo>, Error>;
    async fn addr_list_v4(&self) -> Result<Vec<AddrInfo>, Error>;
}

// ============================================================================
// Real Implementation
// ============================================================================

/// Production [`NetlinkOps`] backed by an `rtnetlink::Handle`.
#[derive(Clone)]
pub struct RealNetlinkOps {
    handle: rtnetlink::Handle,
}

impl RealNetlinkOps {
    pub fn new(handle: rtnetlink::Handle) -> Self {
        Self { handle }
    }

    /// Expose the underlying handle for callers that need direct access
    /// (e.g. main.rs spawning the connection task).
    pub fn handle(&self) -> &rtnetlink::Handle {
        &self.handle
    }

    fn build_route_message(
        spec: &RouteSpec,
    ) -> rtnetlink::packet_route::route::RouteMessage {
        let mut builder = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(spec.destination, spec.prefix_len)
            .output_interface(spec.if_index);
        if let Some(gw) = spec.gateway {
            builder = builder.gateway(gw);
        }
        if let Some(id) = spec.table_id {
            builder = builder.table_id(id);
        }
        builder.build()
    }

    fn route_message_to_spec(
        msg: &rtnetlink::packet_route::route::RouteMessage,
    ) -> Option<RouteSpec> {
        let if_index = msg
            .attributes
            .iter()
            .find_map(|a| match a {
                RouteAttribute::Oif(idx) => Some(*idx),
                _ => None,
            })?;

        let destination = msg
            .attributes
            .iter()
            .find_map(|a| match a {
                RouteAttribute::Destination(RouteAddress::Inet(ip)) => Some(*ip),
                _ => None,
            })
            .unwrap_or(Ipv4Addr::UNSPECIFIED);

        let gateway = msg.attributes.iter().find_map(|a| match a {
            RouteAttribute::Gateway(RouteAddress::Inet(ip)) => Some(*ip),
            _ => None,
        });

        let table_id = msg.attributes.iter().find_map(|a| match a {
            RouteAttribute::Table(id) => Some(*id),
            _ => None,
        });

        Some(RouteSpec {
            destination,
            prefix_len: msg.header.destination_prefix_length,
            gateway,
            if_index,
            table_id,
        })
    }
}

#[async_trait]
impl NetlinkOps for RealNetlinkOps {
    async fn route_add(&self, route: &RouteSpec) -> Result<(), Error> {
        let msg = Self::build_route_message(route);
        self.handle.route().add(msg).execute().await?;
        Ok(())
    }

    async fn route_replace(&self, route: &RouteSpec) -> Result<(), Error> {
        let msg = Self::build_route_message(route);
        self.handle.route().add(msg).replace().execute().await?;
        Ok(())
    }

    async fn route_del(&self, route: &RouteSpec) -> Result<(), Error> {
        let msg = Self::build_route_message(route);
        self.handle.route().del(msg).execute().await?;
        Ok(())
    }

    async fn route_list(&self, table_id: Option<u32>) -> Result<Vec<RouteSpec>, Error> {
        let mut builder = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default();
        if let Some(id) = table_id {
            builder = builder.table_id(id);
        }
        let routes: Vec<_> = self
            .handle
            .route()
            .get(builder.build())
            .execute()
            .try_collect()
            .await?;

        Ok(routes.iter().filter_map(Self::route_message_to_spec).collect())
    }

    async fn rule_add(&self, rule: &RuleSpec) -> Result<(), Error> {
        use rtnetlink::packet_route::rule::RuleAction;
        self.handle
            .rule()
            .add()
            .v4()
            .fw_mark(rule.fw_mark)
            .priority(rule.priority)
            .table_id(rule.table_id)
            .action(RuleAction::ToTable)
            .execute()
            .await?;
        Ok(())
    }

    async fn rule_del(&self, rule: &RuleSpec) -> Result<(), Error> {
        // Find the matching rule message and delete it
        let rules: Vec<_> = self
            .handle
            .rule()
            .get(rtnetlink::IpVersion::V4)
            .execute()
            .try_collect()
            .await?;

        for msg in rules {
            let has_mark = msg
                .attributes
                .iter()
                .any(|a| matches!(a, RuleAttribute::FwMark(m) if *m == rule.fw_mark));
            let has_table = msg
                .attributes
                .iter()
                .any(|a| matches!(a, RuleAttribute::Table(t) if *t == rule.table_id));

            if has_mark && has_table {
                self.handle.rule().del(msg).execute().await?;
                return Ok(());
            }
        }

        Err(Error::General(format!(
            "rule not found: fw_mark={:#x} table={}",
            rule.fw_mark, rule.table_id
        )))
    }

    async fn rule_list_v4(&self) -> Result<Vec<RuleSpec>, Error> {
        let rules: Vec<_> = self
            .handle
            .rule()
            .get(rtnetlink::IpVersion::V4)
            .execute()
            .try_collect()
            .await?;

        Ok(rules
            .iter()
            .filter_map(|msg| {
                let fw_mark = msg.attributes.iter().find_map(|a| match a {
                    RuleAttribute::FwMark(m) => Some(*m),
                    _ => None,
                })?;
                let table_id = msg.attributes.iter().find_map(|a| match a {
                    RuleAttribute::Table(t) => Some(*t),
                    _ => None,
                })?;
                let priority = msg.attributes.iter().find_map(|a| match a {
                    RuleAttribute::Priority(p) => Some(*p),
                    _ => None,
                }).unwrap_or(0);

                Some(RuleSpec {
                    fw_mark,
                    table_id,
                    priority,
                })
            })
            .collect())
    }

    async fn link_list(&self) -> Result<Vec<LinkInfo>, Error> {
        let links: Vec<_> = self
            .handle
            .link()
            .get()
            .execute()
            .try_collect()
            .await?;

        Ok(links
            .iter()
            .filter_map(|link| {
                let name = link.attributes.iter().find_map(|a| match a {
                    LinkAttribute::IfName(n) => Some(n.clone()),
                    _ => None,
                })?;
                Some(LinkInfo {
                    index: link.header.index,
                    name,
                })
            })
            .collect())
    }

    async fn addr_list_v4(&self) -> Result<Vec<AddrInfo>, Error> {
        let addrs: Vec<_> = self
            .handle
            .address()
            .get()
            .execute()
            .try_collect()
            .await?;

        Ok(addrs
            .iter()
            .filter_map(|addr| {
                let ip = addr.attributes.iter().find_map(|a| match a {
                    AddressAttribute::Address(IpAddr::V4(ip)) => Some(*ip),
                    _ => None,
                })?;
                Some(AddrInfo {
                    if_index: addr.header.index,
                    addr: ip,
                })
            })
            .collect())
    }
}
