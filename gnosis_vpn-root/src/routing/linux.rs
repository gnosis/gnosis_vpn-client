use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{event, wireguard, worker};

use super::{Error, Routing};
use crate::wg_tooling;
use futures::TryStreamExt;
use rtnetlink::IpVersion;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::rule::{RuleAction, RuleAttribute};
use std::net::Ipv4Addr;
use std::str::FromStr;

pub fn build_userspace_router(worker: worker::Worker, wg_data: event::WireGuardData) -> Result<Router, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn); // Task terminates once the Router is dropped
    Ok(Router {
        worker,
        wg_data,
        handle,
        if_indices: None,
    })
}

pub fn static_fallback_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> impl Routing {
    FallbackRouter { wg_data, peer_ips }
}

#[derive(Debug, Copy, Clone)]
struct NetworkDeviceInfo {
    /// Index of the WAN interface
    wan_if_index: u32,
    /// Default gateway of the WAN interface
    wan_gw: Ipv4Addr,
    /// Index of the VPN interface
    vpn_if_index: u32,
    /// Default gateway of the VPN interface
    vpn_gw: Ipv4Addr,
    /// CIDR of the VPN subnet
    vpn_cidr: cidr::Ipv4Cidr,
}

impl NetworkDeviceInfo {
    const VPN_SUBNET_PREFIX: u8 = 9;

    async fn get_via_rtnetlink(handle: &rtnetlink::Handle, vpn_ip: &str) -> Result<Self, Error> {
        let vpn_gw = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(vpn_ip, Ipv4Addr::from_str)
            .map_err(|e| Error::General(format!("invalid wg interface address {e}")))?;

        if !vpn_gw.is_host_address() {
            return Err(Error::General("vpn gateway must be a host address".into()));
        }
        let vpn_gw = vpn_gw.first_address();

        // The default route is the one with the longest prefix match (= smallest prefix length)
        let default_route = handle
            .route()
            .get(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().build())
            .execute()
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .min_by_key(|route| route.header.destination_prefix_length)
            .ok_or(Error::NoInterface)?;

        let wan_if_index = default_route
            .attributes
            .iter()
            .find_map(|attr| match attr {
                rtnetlink::packet_route::route::RouteAttribute::Oif(index) => Some(*index),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        let wan_gw = default_route
            .attributes
            .iter()
            .find_map(|attr| match attr {
                rtnetlink::packet_route::route::RouteAttribute::Gateway(
                    rtnetlink::packet_route::route::RouteAddress::Inet(gw),
                ) => Some(*gw),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        let vpn_if_index = handle
            .link()
            .get()
            .execute()
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .find_map(|link| {
                link.attributes.iter().find_map(|attr| match attr {
                    LinkAttribute::IfName(if_name) if if_name == wireguard::WG_INTERFACE => Some(link.header.index),
                    _ => None,
                })
            })
            .ok_or(Error::NoInterface)?;

        Ok(Self {
            wan_if_index,
            wan_gw,
            vpn_if_index,
            vpn_gw,
            vpn_cidr: cidr::Cidr::new(vpn_gw, Self::VPN_SUBNET_PREFIX)
                .map_err(|_| Error::General("invalid vpn subnet range".into()))?,
        })
    }
}

pub struct Router {
    worker: worker::Worker,
    wg_data: event::WireGuardData,
    // Once dropped, the spawned rtnetlink task will terminate
    handle: rtnetlink::Handle,
    if_indices: Option<NetworkDeviceInfo>,
}

pub struct FallbackRouter {
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

// FwMark for traffic the does not go through the VPN
const FW_MARK: u32 = 0xFEED_CAFE;

// Table for traffic that does not go through the VPN
const TABLE_ID: u32 = 108;

// Priority of the FwMark routing table rule
const RULE_PRIORITY: u32 = 1;

const IP_TABLE: &str = "mangle";
const IP_CHAIN: &str = "OUTPUT";

/// Creates `iptables` rules to mark all traffic from the VPN user with `FW_MARK`
/// This is currently a temporary solution until the fwmark can be set explicit on the libp2p socket in hopr-lib.
///
/// Equivalent commands:
/// 1. `iptables -t mangle -F OUTPUT`
/// 2. `iptables -t mangle -A OUTPUT -m owner --uid-owner $VPN_UID -o lo -j RETURN`
/// 3. `iptables -t mangle -A OUTPUT -m owner --uid-owner $VPN_UID -j MARK --set-mark $FW_MARK`
fn setup_iptables(vpn_uid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    if iptables.chain_exists(IP_TABLE, IP_CHAIN)? {
        iptables.flush_chain(IP_TABLE, IP_CHAIN)?;
    } else {
        iptables.new_chain(IP_TABLE, IP_CHAIN)?;
    }

    // Keep loopback for VPN user unmarked
    iptables.append(
        IP_TABLE,
        IP_CHAIN,
        &format!("-m owner --uid-owner {vpn_uid} -o lo -j RETURN"),
    )?;
    // Mark all other traffic from VPN user
    iptables.append(
        IP_TABLE,
        IP_CHAIN,
        &format!("-m owner --uid-owner {vpn_uid} -j MARK --set-mark {FW_MARK}"),
    )?;

    Ok(())
}

fn flush_ip_tables() -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    iptables.flush_chain(IP_TABLE, IP_CHAIN)?;
    Ok(())
}

/// Linux-specific implementation of [`Routing`] for split-tunnel routing.
#[async_trait]
impl Routing for Router {
    /// Install split-tunnel routing.
    ///
    /// The steps:
    ///   1. Generate wg-quick config and run `wg-quick up`
    ///      The `wg-quick` config makes sure that WG UDP packets have the same fwmark set and that it sets no additional routing rules.
    ///   2. Set all traffic from the VPN user to be marked with the fwmark
    ///      This is currently done via `iptables` rule, but it will be replaced with an explicit fwmark on the hopr-lib transport socket.
    ///      See [`setup_iptables`] for details.
    ///   3. Create a new routing table for traffic that does not go through the VPN (TABLE_ID)
    ///      Equivalent command: `ip route add default dev "$IF_WAN" table "$TABLE_ID"`
    ///   4. Allow non-VPN traffic to reach VPN addresses
    ///      Equivalent command: `ip route add $VPN_RANGE dev "$IP_VPN" table "$TABLE_ID"`
    ///   5. Add a rule to direct traffic with the specified fwmark to the new routing table
    ///      Equivalent command: `ip rule add mark $FW_MARK table $TABLE_ID pref 1`
    ///   6. Adjust the default routing table (MAIN) to use the VPN interface for default routing
    ///      Equivalent command: `ip route replace default dev "$IF_VPN"`
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.if_indices.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Generate wg quick content
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            true,
            Some(["Table = off".to_string()].into_iter().collect()),
        );
        // Run wg-quick up
        wg_tooling::up(wg_quick_content).await?;
        tracing::debug!("wg-quick up");

        // Obtain network interface data
        let ifs = NetworkDeviceInfo::get_via_rtnetlink(&self.handle, &self.wg_data.interface_info.address).await?;
        let NetworkDeviceInfo {
            wan_if_index,
            wan_gw,
            vpn_if_index,
            vpn_gw,
            vpn_cidr,
        } = ifs;
        self.if_indices = Some(ifs);
        tracing::debug!(?ifs, "interface data");

        // This steps marks all traffic from VPN_USER with FW_MARK
        // Remove this once we can set the fwmark directly on the libp2p Socket
        setup_iptables(self.worker.uid).map_err(Error::iptables)?;
        tracing::debug!(uid = self.worker.uid, "iptables rules set up");

        // New routing table TABLE_ID: All traffic in this table goes to the WAN interface (bypasses VPN)
        let no_vpn_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .table_id(TABLE_ID)
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_if_index)
            .gateway(wan_gw)
            .build();
        self.handle.route().add(no_vpn_route).execute().await?;
        tracing::debug!("ip route add default via {wan_gw} dev {wan_if_index} table {TABLE_ID}");

        // Allow VPN traffic arriving to the TABLE_ID table goes to the VPN interface
        let vpn_addrs_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .table_id(TABLE_ID)
            .destination_prefix(vpn_cidr.first_address(), vpn_cidr.network_length())
            .output_interface(vpn_if_index)
            .gateway(vpn_gw)
            .build();
        self.handle.route().add(vpn_addrs_route).execute().await?;
        tracing::debug!("ip route add {vpn_cidr} via {vpn_gw} dev {vpn_if_index}");

        // Add rule: everything marked with FW_MARK goes via the TABLE_ID routing table
        self.handle
            .rule()
            .add()
            .v4()
            .fw_mark(FW_MARK)
            .priority(RULE_PRIORITY)
            .table_id(TABLE_ID)
            .action(RuleAction::ToTable)
            .execute()
            .await?;
        tracing::debug!("ip rule add mark {FW_MARK} table {TABLE_ID} pref {RULE_PRIORITY}");

        // Adjust the main routing table so that everything gets routed via the VPN interface
        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(vpn_if_index)
            .build();
        self.handle.route().add(default_route).execute().await?;
        tracing::debug!("ip route add default dev {vpn_if_index}");

        Ok(())
    }

    /// Uninstalls the split-tunnel routing.
    ///
    /// The steps:
    ///   1. Replace the default route in the MAIN routing table
    ///      Equivalent command: `ip route replace default dev "$IF_WAN"`
    ///   2. Delete the mark rule for the TABLE_ID
    ///      Equivalent command: `ip rule del mark $FW_MARK table $TABLE_ID`
    ///   3. Delete the TABLE_ID routing table
    ///      Equivalent command: `ip route del $VPN_RANGE dev "$VPN_WAN" table "$TABLE_ID"`
    ///   4. Delete the TABLE_ID routing table
    ///      Equivalent command: `ip route del default dev "$IF_WAN" table "$TABLE_ID"`
    ///   5. Remove the `iptables` rules. This is temporary until hopr-lib supports explicit fwmark on the transport socket.
    ///   6. Run `wg-quick down`
    ///
    async fn teardown(&mut self) -> Result<(), Error> {
        let NetworkDeviceInfo {
            wan_if_index,
            vpn_if_index,
            vpn_gw,
            vpn_cidr,
            ..
        } = self
            .if_indices
            .take()
            .ok_or(Error::General("invalid state: not set up".into()))?;

        // Set the default route back to the WAN interface
        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_if_index)
            .build();

        if let Err(error) = self.handle.route().add(default_route).execute().await {
            tracing::error!(%error, "failed to set default route back to interface, continuing anyway");
        } else {
            tracing::debug!("ip route add default via {vpn_gw} dev {wan_if_index}");
        }

        // Delete the fwmark routing table rule
        if let Ok(rules) = self
            .handle
            .rule()
            .get(IpVersion::V4)
            .execute()
            .try_collect::<Vec<_>>()
            .await
        {
            for rule in rules.into_iter().filter(|rule| {
                rule.attributes
                    .iter()
                    .any(|a| matches!(a, RuleAttribute::FwMark(fwmark) if fwmark == &FW_MARK))
                    && rule
                        .attributes
                        .iter()
                        .any(|a| matches!(a, RuleAttribute::Table(table) if table == &TABLE_ID))
            }) {
                if let Err(error) = self.handle.rule().del(rule).execute().await {
                    tracing::error!(%error, "failed to delete fwmark routing table rule, continuing anyway");
                } else {
                    tracing::debug!("ip rule del mark {FW_MARK} table {TABLE_ID}");
                }
            }
        }

        // Delete the TABLE_ID routing table VPN route
        if let Err(error) = self
            .handle
            .route()
            .del(
                rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
                    .table_id(TABLE_ID)
                    .destination_prefix(vpn_cidr.first_address(), vpn_cidr.network_length())
                    .output_interface(vpn_if_index)
                    .build(),
            )
            .execute()
            .await
        {
            tracing::error!(%error, "failed to delete table {TABLE_ID}, continuing anyway");
        } else {
            tracing::debug!("ip route del {vpn_cidr} via {vpn_gw} dev {vpn_if_index}");
        }

        // Delete the TABLE_ID routing table default route
        if let Err(error) = self
            .handle
            .route()
            .del(
                rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
                    .table_id(TABLE_ID)
                    .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
                    .output_interface(wan_if_index)
                    .build(),
            )
            .execute()
            .await
        {
            tracing::error!(%error, "failed to delete table {TABLE_ID}, continuing anyway");
        } else {
            tracing::debug!("ip route del default via {vpn_gw} dev {wan_if_index}");
        }

        // Flush the iptables rules
        if let Err(error) = flush_ip_tables().map_err(Error::iptables) {
            tracing::error!(%error, "failed to flush iptables rules, continuing anyway");
        }
        tracing::debug!("iptables rules flushed");

        // Run wg-quick down
        wg_tooling::down().await?;
        tracing::debug!("wg-quick down");

        Ok(())
    }
}

#[async_trait]
impl Routing for FallbackRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        let interface_gateway = interface().await?;
        let mut extra = self
            .peer_ips
            .iter()
            .map(|ip| pre_up_routing(ip, interface_gateway.clone()))
            .collect::<Vec<String>>();
        extra.extend(
            self.peer_ips
                .iter()
                .map(|ip| post_down_routing(ip, interface_gateway.clone()))
                .collect::<Vec<String>>(),
        );

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, Some(extra));
        wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&mut self) -> Result<(), Error> {
        wg_tooling::down().await?;
        Ok(())
    }
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    // TODO: rewrite via rtnetlink
    match gateway {
        Some(gw) => format!(
            "PreUp = ip route add {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device
        ),
        None => format!(
            "PreUp = ip route add {relayer_ip} dev {device}",
            relayer_ip = relayer_ip,
            device = device
        ),
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    // TODO: rewrite via rtnetlink
    match gateway {
        Some(gw) => format!(
            "PostDown = ip route del {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device,
        ),
        None => format!(
            "PostDown = ip route del {relayer_ip} dev {device}",
            relayer_ip = relayer_ip,
            device = device,
        ),
    }
}

async fn interface() -> Result<(String, Option<String>), Error> {
    // TODO: rewrite via rtnetlink
    let output = Command::new("ip")
        .arg("route")
        .arg("show")
        .arg("default")
        .run_stdout()
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();

    let device_index = parts.iter().position(|&x| x == "dev");
    let via_index = parts.iter().position(|&x| x == "via");

    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
    Ok((device, gateway))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    #[test]
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = "default via 192.168.101.1 dev wlp2s0 proto dhcp src 192.168.101.202 metric 600 ";

        let (device, gateway) = super::parse_interface(output)?;

        assert_eq!(device, "wlp2s0");
        assert_eq!(gateway, Some("192.168.101.1".to_string()));
        Ok(())
    }

    #[test]
    fn test_parse_cidr() -> anyhow::Result<()> {
        let cidr = "192.168.101.0/24";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 0));
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.32/24";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 0));
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.1";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 1));
        assert_eq!(ip.network_length(), 32);
        assert_eq!("192.168.101.1", ip.to_string());

        Ok(())
    }
}
