use async_trait::async_trait;
use futures::TryStreamExt;
use rtnetlink::IpVersion;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::rule::{RuleAction, RuleAttribute};
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{event, wireguard, worker};

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::str::FromStr;

use super::{Error, Routing};
use crate::wg_tooling;

pub fn dynamic_router(
    state_home: PathBuf,
    worker: worker::Worker,
    wg_data: event::WireGuardData,
) -> Result<Router, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn); // Task terminates once the Router is dropped
    Ok(Router {
        state_home,
        worker,
        wg_data,
        handle,
        if_indices: None,
    })
}

pub fn static_fallback_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> impl Routing {
    FallbackRouter {
        state_home,
        wg_data,
        peer_ips,
    }
}

#[derive(Debug, Clone)]
struct NetworkDeviceInfo {
    /// Index of the WAN interface
    wan_if_index: u32,
    /// Name of the WAN interface (e.g. "eth0")
    wan_if_name: String,
    /// Default gateway of the WAN interface
    wan_gw: Ipv4Addr,
    /// Index of the VPN interface
    vpn_if_index: u32,
    /// Default gateway of the VPN interface
    vpn_gw: Ipv4Addr,
    /// CIDR of the VPN subnet
    vpn_cidr: cidr::Ipv4Cidr,
}

/// WAN interface information gathered before VPN interface exists.
#[derive(Debug, Clone)]
struct WanInfo {
    if_index: u32,
    if_name: String,
    gateway: Ipv4Addr,
}

/// VPN interface information gathered after `wg-quick up`.
#[derive(Debug, Clone)]
struct VpnInfo {
    if_index: u32,
    gateway: Ipv4Addr,
    cidr: cidr::Ipv4Cidr,
}

impl NetworkDeviceInfo {
    /// Construct `NetworkDeviceInfo` from separately gathered WAN and VPN info.
    fn from_parts(wan: WanInfo, vpn: VpnInfo) -> Self {
        Self {
            wan_if_index: wan.if_index,
            wan_if_name: wan.if_name,
            wan_gw: wan.gateway,
            vpn_if_index: vpn.if_index,
            vpn_gw: vpn.gateway,
            vpn_cidr: vpn.cidr,
        }
    }

    /// Get WAN interface info via rtnetlink.
    /// Can be called before VPN interface exists.
    async fn get_wan_info_via_rtnetlink(handle: &rtnetlink::Handle) -> Result<WanInfo, Error> {
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

        let if_index = default_route
            .attributes
            .iter()
            .find_map(|attr| match attr {
                rtnetlink::packet_route::route::RouteAttribute::Oif(index) => Some(*index),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        let gateway = default_route
            .attributes
            .iter()
            .find_map(|attr| match attr {
                rtnetlink::packet_route::route::RouteAttribute::Gateway(
                    rtnetlink::packet_route::route::RouteAddress::Inet(gw),
                ) => Some(*gw),
                _ => None,
            })
            .ok_or(Error::NoInterface)?;

        let links: Vec<_> = handle.link().get().execute().try_collect::<Vec<_>>().await?;

        let if_name = links
            .iter()
            .find_map(|link| {
                if link.header.index == if_index {
                    link.attributes.iter().find_map(|attr| match attr {
                        LinkAttribute::IfName(name) => Some(name.clone()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::General(format!("WAN interface name not found for index {if_index}")))?;

        Ok(WanInfo {
            if_index,
            if_name,
            gateway,
        })
    }

    /// Get VPN interface info via rtnetlink.
    /// Must be called after `wg-quick up` creates the VPN interface.
    async fn get_vpn_info_via_rtnetlink(
        handle: &rtnetlink::Handle,
        vpn_ip: &str,
        vpn_prefix: u8,
    ) -> Result<VpnInfo, Error> {
        let vpn_client_ip_cidr: cidr::Ipv4Cidr =
            cidr::parsers::parse_cidr_ignore_hostbits(vpn_ip, Ipv4Addr::from_str)
                .map_err(|e| Error::General(format!("invalid wg interface address {e}")))?;

        // This must be a unique VPN client address, not a block of addresses
        if !vpn_client_ip_cidr.is_host_address() {
            return Err(Error::General("vpn gateway must be a host address".into()));
        }

        // Construct VPN subnet CIDR by ignoring the host bits of the VPN client IP and using the default prefix length
        let cidr: cidr::Ipv4Cidr = cidr::parsers::parse_cidr_ignore_hostbits(
            &format!("{}/{}", vpn_client_ip_cidr.first_address(), vpn_prefix),
            Ipv4Addr::from_str,
        )
        .map_err(|_| Error::General("invalid vpn subnet range".into()))?;

        let links: Vec<_> = handle.link().get().execute().try_collect::<Vec<_>>().await?;

        let if_index = links
            .iter()
            .find_map(|link| {
                link.attributes.iter().find_map(|attr| match attr {
                    LinkAttribute::IfName(name) if name == wireguard::WG_INTERFACE => Some(link.header.index),
                    _ => None,
                })
            })
            .ok_or(Error::NoInterface)?;

        // Gateway of the VPN interface is the second address in the VPN subnet
        let gateway = cidr
            .iter()
            .addresses()
            .nth(1)
            .ok_or(Error::General("invalid vpn subnet range".into()))?;

        Ok(VpnInfo {
            if_index,
            gateway,
            cidr,
        })
    }
}

pub struct Router {
    state_home: PathBuf,
    worker: worker::Worker,
    wg_data: event::WireGuardData,
    // Once dropped, the spawned rtnetlink task will terminate
    handle: rtnetlink::Handle,
    if_indices: Option<NetworkDeviceInfo>,
}

pub struct FallbackRouter {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

// FwMark for traffic the does not go through the VPN
const FW_MARK: u32 = 0xFEED_CAFE;

// Table for traffic that does not go through the VPN
const TABLE_ID: u32 = 108;

// Priority of the FwMark routing table rule
const RULE_PRIORITY: u32 = 1;

// Subnet prefix length for the VPN subnet, @esterlus this might need to be configurable
const VPN_SUBNET_PREFIX: u8 = 9;

const IP_TABLE: &str = "mangle";
const IP_CHAIN: &str = "OUTPUT";

const NAT_TABLE: &str = "nat";
const NAT_CHAIN: &str = "POSTROUTING";

/// Creates `iptables` rules to mark all traffic from the VPN user with `FW_MARK`
/// and to NAT-masquerade that traffic on the WAN interface.
/// This is currently a temporary solution until the fwmark can be set explicit on the libp2p socket in hopr-lib.
///
/// Equivalent commands:
/// 1. `iptables -t mangle -F OUTPUT`
/// 2. `iptables -t mangle -A OUTPUT -m owner --uid-owner $VPN_UID -o lo -j RETURN`
/// 3. `iptables -t mangle -A OUTPUT -m owner --uid-owner $VPN_UID -j MARK --set-mark $FW_MARK`
/// 4. `iptables -t nat -A POSTROUTING -m mark --mark $FW_MARK -o $WAN_IF -j MASQUERADE`
fn setup_iptables(vpn_uid: u32, wan_if_name: &str) -> Result<(), Box<dyn std::error::Error>> {
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

    // Rewrite the source address of bypassed (marked) traffic leaving via the WAN interface.
    // Without this, packets retain the VPN subnet source IP and the upstream gateway drops them
    // because it has no return route for that subnet.
    let nat_rule = format!("-m mark --mark {FW_MARK} -o {wan_if_name} -j MASQUERADE");
    // Delete any stale rule first (e.g. left over from a previous crash) to avoid duplicates.
    // Unlike the mangle chain we cannot flush nat POSTROUTING because other services use it.
    if iptables.exists(NAT_TABLE, NAT_CHAIN, &nat_rule)? {
        iptables.delete(NAT_TABLE, NAT_CHAIN, &nat_rule)?;
    }
    iptables.append(NAT_TABLE, NAT_CHAIN, &nat_rule)?;

    Ok(())
}

fn teardown_iptables(wan_if_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    iptables.flush_chain(IP_TABLE, IP_CHAIN)?;

    // Delete only our specific NAT rule rather than flushing the entire nat POSTROUTING chain,
    // because other services (Docker, libvirt, etc.) install their own rules there.
    let nat_rule = format!("-m mark --mark {FW_MARK} -o {wan_if_name} -j MASQUERADE");
    if iptables.exists(NAT_TABLE, NAT_CHAIN, &nat_rule)? {
        iptables.delete(NAT_TABLE, NAT_CHAIN, &nat_rule)?;
    }

    Ok(())
}

impl Router {
    /// Rollback Phase 1 setup (fwmark rule, TABLE_ID route, iptables).
    /// Used when Phase 2 or Phase 3 fails.
    async fn rollback_phase1(&self, wan_info: &WanInfo) {
        // Delete fwmark rule
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
                let _ = self.handle.rule().del(rule).execute().await;
            }
        }

        // Delete TABLE_ID default route
        let _ = self
            .handle
            .route()
            .del(
                rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
                    .table_id(TABLE_ID)
                    .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
                    .output_interface(wan_info.if_index)
                    .gateway(wan_info.gateway)
                    .build(),
            )
            .execute()
            .await;

        // Remove iptables rules
        let _ = teardown_iptables(&wan_info.if_name);
    }
}

/// Linux-specific implementation of [`Routing`] for split-tunnel routing.
#[async_trait]
impl Routing for Router {
    /// Install split-tunnel routing.
    ///
    /// The setup is split into phases to avoid a race condition where HOPR p2p connections
    /// could briefly drop when the WireGuard interface comes up. By establishing bypass
    /// routing rules BEFORE creating the VPN interface, marked HOPR traffic always uses
    /// the WAN interface, even during the VPN setup window.
    ///
    /// Phase 1 (before wg-quick up):
    ///   1. Get WAN interface info (index, name, gateway)
    ///   2. Set up iptables rules to mark HOPR traffic with FW_MARK
    ///   3. Create TABLE_ID routing table with WAN as default gateway
    ///   4. Add fwmark rule: marked traffic uses TABLE_ID (bypasses VPN)
    ///
    /// Phase 2:
    ///   5. Run wg-quick up (safe now - HOPR traffic is already protected)
    ///
    /// Phase 3 (after wg-quick up):
    ///   6. Get VPN interface info
    ///   7. Add VPN subnet route to TABLE_ID (so bypassed traffic can reach VPN peers)
    ///   8. Replace main default route to VPN interface (all other traffic uses VPN)
    ///   9. Flush routing cache
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.if_indices.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // === PHASE 1: Setup bypass routing BEFORE wg-quick up ===
        // This prevents HOPR traffic from being routed through the nascent VPN interface.

        // Step 1: Get WAN interface info (VPN doesn't exist yet)
        let wan_info = NetworkDeviceInfo::get_wan_info_via_rtnetlink(&self.handle).await?;
        tracing::debug!(?wan_info, "WAN interface data");

        // Step 2: Setup iptables rules to mark HOPR traffic for bypass
        // Remove this once we can set the fwmark directly on the libp2p Socket
        setup_iptables(self.worker.uid, &wan_info.if_name).map_err(Error::iptables)?;
        tracing::debug!(uid = self.worker.uid, "iptables rules set up");

        // Step 3: Create TABLE_ID with WAN default route
        // All traffic in this table bypasses VPN and goes directly to WAN
        let no_vpn_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .table_id(TABLE_ID)
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_info.if_index)
            .gateway(wan_info.gateway)
            .build();
        if let Err(e) = self.handle.route().add(no_vpn_route).execute().await {
            // Rollback iptables on failure
            let _ = teardown_iptables(&wan_info.if_name);
            return Err(e.into());
        }
        tracing::debug!(
            "ip route add default via {} dev {} table {TABLE_ID}",
            wan_info.gateway,
            wan_info.if_index
        );

        // Step 4: Add fwmark rule - marked traffic goes to TABLE_ID
        if let Err(e) = self
            .handle
            .rule()
            .add()
            .v4()
            .fw_mark(FW_MARK)
            .priority(RULE_PRIORITY)
            .table_id(TABLE_ID)
            .action(RuleAction::ToTable)
            .execute()
            .await
        {
            // Rollback TABLE_ID route and iptables on failure
            let _ = self
                .handle
                .route()
                .del(
                    rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
                        .table_id(TABLE_ID)
                        .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
                        .output_interface(wan_info.if_index)
                        .gateway(wan_info.gateway)
                        .build(),
                )
                .execute()
                .await;
            let _ = teardown_iptables(&wan_info.if_name);
            return Err(e.into());
        }
        tracing::debug!("ip rule add mark {FW_MARK} table {TABLE_ID} pref {RULE_PRIORITY}");

        // === PHASE 2: Now safe to bring up WireGuard ===
        // HOPR traffic is already protected by the bypass rules above.

        // Step 5: Generate wg-quick config and run wg-quick up
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            vec!["Table = off".to_string()],
        );
        if let Err(e) = wg_tooling::up(self.state_home.clone(), wg_quick_content).await {
            // Rollback Phase 1 setup on failure
            self.rollback_phase1(&wan_info).await;
            return Err(e);
        }
        tracing::debug!("wg-quick up");

        // === PHASE 3: Complete routing with VPN interface info ===

        // Step 6: Get VPN interface info
        let vpn_info = match NetworkDeviceInfo::get_vpn_info_via_rtnetlink(
            &self.handle,
            &self.wg_data.interface_info.address,
            VPN_SUBNET_PREFIX,
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                // Rollback: bring down WG and cleanup Phase 1
                let _ = wg_tooling::down(self.state_home.clone(), gnosis_vpn_lib::shell_command_ext::Logs::Omit).await;
                self.rollback_phase1(&wan_info).await;
                return Err(e);
            }
        };
        tracing::debug!(?vpn_info, "VPN interface data");

        // Store combined info for teardown
        self.if_indices = Some(NetworkDeviceInfo::from_parts(wan_info.clone(), vpn_info.clone()));

        // Step 7: Add VPN subnet route to TABLE_ID
        // This allows bypassed traffic to still reach VPN addresses
        let vpn_addrs_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .table_id(TABLE_ID)
            .destination_prefix(vpn_info.cidr.first_address(), vpn_info.cidr.network_length())
            .output_interface(vpn_info.if_index)
            .build();
        if let Err(e) = self.handle.route().add(vpn_addrs_route).execute().await {
            // Rollback: bring down WG and cleanup Phase 1
            self.if_indices = None;
            let _ = wg_tooling::down(self.state_home.clone(), gnosis_vpn_lib::shell_command_ext::Logs::Omit).await;
            self.rollback_phase1(&wan_info).await;
            return Err(e.into());
        }
        tracing::debug!(
            "ip route add {} dev {} table {TABLE_ID}",
            vpn_info.cidr,
            vpn_info.if_index
        );

        // Step 8: Replace main default route to VPN interface
        // All non-bypassed traffic now goes through VPN
        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(vpn_info.if_index)
            .build();
        if let Err(e) = self.handle.route().add(default_route).execute().await {
            // Rollback: remove VPN subnet route, bring down WG and cleanup Phase 1
            let _ = self
                .handle
                .route()
                .del(
                    rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
                        .table_id(TABLE_ID)
                        .destination_prefix(vpn_info.cidr.first_address(), vpn_info.cidr.network_length())
                        .output_interface(vpn_info.if_index)
                        .build(),
                )
                .execute()
                .await;
            self.if_indices = None;
            let _ = wg_tooling::down(self.state_home.clone(), gnosis_vpn_lib::shell_command_ext::Logs::Omit).await;
            self.rollback_phase1(&wan_info).await;
            return Err(e.into());
        }
        tracing::debug!("ip route add default dev {}", vpn_info.if_index);

        // Step 9: Flush routing cache
        flush_routing_cache().await?;
        tracing::debug!("flushed routing cache");

        tracing::info!("routing is ready");
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
    ///   5. Flush the routing table cache
    ///      Equivalent command: `ip route flush cache`
    ///   6. Remove the `iptables` mangle and NAT rules. This is temporary until hopr-lib supports explicit fwmark on the transport socket.
    ///   7. Run `wg-quick down`
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        let NetworkDeviceInfo {
            wan_if_index,
            wan_if_name,
            vpn_if_index,
            vpn_gw,
            vpn_cidr,
            wan_gw,
        } = self
            .if_indices
            .take()
            .ok_or(Error::General("invalid state: not set up".into()))?;

        // Set the default route back to the WAN interface
        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_if_index)
            .gateway(wan_gw)
            .build();

        if let Err(error) = self.handle.route().add(default_route).execute().await {
            tracing::error!(%error, "failed to set default route back to interface, continuing anyway");
        } else {
            tracing::debug!("ip route add default via {wan_gw} dev {wan_if_index}");
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
                    .gateway(wan_gw)
                    .build(),
            )
            .execute()
            .await
        {
            tracing::error!(%error, "failed to delete table {TABLE_ID}, continuing anyway");
        } else {
            tracing::debug!("ip route del default via {wan_gw} dev {wan_if_index} table {TABLE_ID}");
        }

        flush_routing_cache().await?;
        tracing::debug!("ip route flush cache");

        // Remove the iptables mangle and NAT rules
        if let Err(error) = teardown_iptables(&wan_if_name).map_err(Error::iptables) {
            tracing::error!(%error, "failed to teardown iptables rules, continuing anyway");
        }
        tracing::debug!("iptables rules removed");

        // Run wg-quick down
        wg_tooling::down(self.state_home.clone(), logs).await?;
        tracing::debug!("wg-quick down");

        Ok(())
    }
}

#[async_trait]
impl Routing for FallbackRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        let interface_gateway = interface().await?;
        let mut extra = vec![];
        for ip in &self.peer_ips {
            extra.extend(pre_up_routing(ip, interface_gateway.clone()));
        }
        for ip in &self.peer_ips {
            extra.push(post_down_routing(ip, interface_gateway.clone()));
        }
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);
        wg_tooling::up(self.state_home.clone(), wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(self.state_home.clone(), logs).await?;
        Ok(())
    }
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> Vec<String> {
    // TODO: rewrite via rtnetlink
    match gateway {
        Some(gw) => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!(
                "PreUp = ip route del {relayer_ip} via {gateway} dev {device} || true",
                relayer_ip = relayer_ip,
                gateway = gw,
                device = device
            ),
            format!(
                "PreUp = ip route add {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gw,
                device = device
            ),
        ],
        None => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!(
                "PreUp = ip route del {relayer_ip} dev {device} || true",
                relayer_ip = relayer_ip,
                device = device
            ),
            format!(
                "PreUp = ip route add {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = device
            ),
        ],
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    // TODO: rewrite via rtnetlink
    match gateway {
        Some(gw) => format!(
            // wg-quick stops execution on error, ignore errors to hit all PostDown commands
            "PostDown = ip route del {relayer_ip} via {gateway} dev {device} || true",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device,
        ),
        None => format!(
            // wg-quick stops execution on error, ignore errors to hit all PostDown commands
            "PostDown = ip route del {relayer_ip} dev {device} || true",
            relayer_ip = relayer_ip,
            device = device,
        ),
    }
}

/// Flushes the routing table cache - this cannot be done via rtnetlink crate.
async fn flush_routing_cache() -> Result<(), Error> {
    Command::new("ip")
        .arg("route")
        .arg("flush")
        .arg("cache")
        .run_stdout(Logs::Print)
        .await?;
    Ok(())
}

async fn interface() -> Result<(String, Option<String>), Error> {
    // TODO: rewrite via rtnetlink
    let output = Command::new("ip")
        .arg("route")
        .arg("show")
        .arg("default")
        .run_stdout(Logs::Print)
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
        assert_eq!(ip.first_address().to_string(), "192.168.101.0");
        assert_eq!(ip.iter().addresses().nth(1).unwrap().to_string(), "192.168.101.1");
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.32/24";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 0));
        assert_eq!(ip.iter().addresses().nth(1).unwrap().to_string(), "192.168.101.1");
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.32/32";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 32));
        assert_eq!(ip.network_length(), 32);
        assert!(ip.is_host_address());
        assert_eq!("192.168.101.32", ip.to_string());

        let cidr = "192.168.101.1";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 168, 101, 1));
        assert_eq!(ip.network_length(), 32);
        assert_eq!("192.168.101.1", ip.to_string());

        let cidr = "192.128.101.33/9";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), Ipv4Addr::new(192, 128, 0, 0));
        assert_eq!(ip.iter().addresses().nth(1).unwrap().to_string(), "192.128.0.1");
        assert_eq!(ip.network_length(), 9);
        assert_eq!("192.128.0.0/9", ip.to_string());

        Ok(())
    }
}
