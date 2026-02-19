//! Linux routing implementation for split-tunnel VPN behavior.
//!
//! Provides two router implementations:
//!
//! ## [`Router`] (Dynamic)
//! Uses rtnetlink and iptables for advanced split-tunnel routing:
//! 1. Sets up iptables rules to mark HOPR traffic with a firewall mark (fwmark)
//! 2. Creates a separate routing table (TABLE_ID) for marked traffic to bypass VPN
//! 3. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 4. Adds VPN subnet route (10.128.0.0/9) to both TABLE_ID and main table
//! 5. Configures default route through VPN for all other traffic
//!
//! ## [`FallbackRouter`] (Static)
//! Simpler implementation using direct `ip route` commands:
//! 1. Adds bypass routes for peer IPs BEFORE bringing up WireGuard (avoids race condition)
//! 2. Adds RFC1918 bypass routes (10.0.0.0/8, etc.) via WAN for LAN access
//! 3. Runs `wg-quick up` with PostUp hook for VPN subnet route (10.128.0.0/9)
//! 4. On teardown, brings down WireGuard first, then cleans up bypass routes
//!
//! Both implementations use a phased approach to avoid race conditions during VPN setup.
//!
//! ## Route Precedence
//! The VPN subnet (10.128.0.0/9) is more specific than the RFC1918 bypass (10.0.0.0/8),
//! so VPN server traffic (e.g. to 10.128.0.1) routes through the tunnel while other
//! RFC1918 Class A traffic bypasses to the WAN for LAN access.

use async_trait::async_trait;
use futures::TryStreamExt;
use rtnetlink::IpVersion;
use rtnetlink::packet_route::address::AddressAttribute;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::rule::{RuleAction, RuleAttribute};

use std::net::IpAddr;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{event, wireguard, worker};

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::str::FromStr;

use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET};

use crate::wg_tooling;

/// Creates a dynamic router using rtnetlink and iptables.
///
/// This is the preferred router on Linux as it provides more robust split-tunnel
/// routing using firewall marks (fwmark) and policy-based routing.
///
/// The router requires pre-existing fwmark infrastructure (set up via
/// `setup_fwmark_infrastructure`) and only handles VPN-specific routing.
pub fn dynamic_router(state_home: PathBuf, wg_data: event::WireGuardData, wan_info: WanInfo) -> Result<Router, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn); // Task terminates once the Router is dropped
    Ok(Router {
        state_home,
        wg_data,
        handle,
        wan_info,
        if_indices: None,
    })
}

/// Creates a static fallback router using direct `ip route` commands.
///
/// Used when dynamic routing is not available. Provides simpler routing
/// by adding explicit host routes for peer IPs before bringing up WireGuard.
pub fn static_fallback_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> impl Routing {
    FallbackRouter {
        state_home,
        wg_data,
        peer_ips,
        bypass_manager: None,
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
pub struct WanInfo {
    pub if_index: u32,
    pub if_name: String,
    pub gateway: Ipv4Addr,
    /// WAN interface's IPv4 address (used for SNAT)
    pub ip_addr: Ipv4Addr,
}

/// Persistent fwmark infrastructure that lives for the daemon's lifetime.
///
/// This struct holds the resources needed for fwmark-based routing bypass.
/// It is created at daemon startup and destroyed at daemon shutdown,
/// independent of individual WireGuard connection lifecycles.
///
/// **Important**: Must be explicitly torn down via `teardown_fwmark_infrastructure()`
/// to clean up iptables rules and routing table entries. Dropping without teardown
/// will log a warning and may leave the system in an inconsistent state.
pub struct FwmarkInfrastructure {
    pub handle: rtnetlink::Handle,
    pub wan_info: WanInfo,
    pub worker_uid: u32,
    /// Tracks whether teardown was called. Set to true when teardown_fwmark_infrastructure() is invoked.
    pub(super) torn_down: bool,
}

impl Drop for FwmarkInfrastructure {
    fn drop(&mut self) {
        if !self.torn_down {
            tracing::warn!(
                "FwmarkInfrastructure dropped without teardown - iptables rules and routing entries may be leaked"
            );
        }
    }
}

/// Sets up the persistent fwmark infrastructure at daemon startup.
///
/// This establishes the iptables rules and routing table entries that
/// allow HOPR traffic to bypass the VPN tunnel. This setup persists
/// for the lifetime of the daemon, independent of individual VPN connections.
///
/// The setup includes:
/// 1. Creating an rtnetlink connection for route management
/// 2. Getting WAN interface info (index, name, gateway)
/// 3. Setting up iptables rules to mark HOPR traffic with FW_MARK
/// 4. Creating TABLE_ID routing table with WAN as default gateway
/// 5. Adding fwmark rule: marked traffic uses TABLE_ID (bypasses VPN)
pub async fn setup_fwmark_infrastructure(worker: &worker::Worker) -> Result<FwmarkInfrastructure, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);

    // Get WAN interface info
    let wan_info = NetworkDeviceInfo::get_wan_info_via_rtnetlink(&handle).await?;
    tracing::debug!(?wan_info, "WAN interface data for fwmark infrastructure");

    // Setup iptables rules to mark HOPR traffic for bypass
    setup_iptables(worker.uid, &wan_info.if_name, wan_info.ip_addr).map_err(Error::iptables)?;
    tracing::debug!(uid = worker.uid, wan_ip = %wan_info.ip_addr, "iptables rules set up");

    // Create TABLE_ID with WAN default route
    let no_vpn_route = default_route(wan_info.if_index, Some(wan_info.gateway), Some(TABLE_ID));
    if let Err(e) = handle.route().add(no_vpn_route).execute().await {
        // Rollback iptables on failure
        if let Err(rollback_err) = teardown_iptables(&wan_info.if_name, wan_info.ip_addr) {
            tracing::warn!(%rollback_err, "rollback failed: could not teardown iptables rules");
        }
        return Err(e.into());
    }
    tracing::debug!(
        "ip route add default via {} dev {} table {TABLE_ID}",
        wan_info.gateway,
        wan_info.if_index
    );

    // Add fwmark rule - marked traffic goes to TABLE_ID
    if let Err(e) = handle
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
        if let Err(rollback_err) = handle
            .route()
            .del(default_route(wan_info.if_index, Some(wan_info.gateway), Some(TABLE_ID)))
            .execute()
            .await
        {
            tracing::warn!(%rollback_err, "rollback failed: could not delete TABLE_ID default route");
        }
        if let Err(rollback_err) = teardown_iptables(&wan_info.if_name, wan_info.ip_addr) {
            tracing::warn!(%rollback_err, "rollback failed: could not teardown iptables rules");
        }
        return Err(e.into());
    }
    tracing::debug!("ip rule add mark {FW_MARK} table {TABLE_ID} pref {RULE_PRIORITY}");

    tracing::info!("fwmark infrastructure is ready");
    Ok(FwmarkInfrastructure {
        handle,
        wan_info,
        worker_uid: worker.uid,
        torn_down: false,
    })
}

/// Tears down the persistent fwmark infrastructure at daemon shutdown.
///
/// This removes the iptables rules and routing table entries that were
/// set up by `setup_fwmark_infrastructure`.
///
/// The teardown includes:
/// 1. Deleting the fwmark routing rule
/// 2. Deleting the TABLE_ID default route
/// 3. Flushing the routing cache
/// 4. Removing iptables mangle and NAT rules
pub async fn teardown_fwmark_infrastructure(mut infra: FwmarkInfrastructure) {
    // Mark as torn down before we start cleanup - this prevents the Drop warning
    infra.torn_down = true;
    let FwmarkInfrastructure { handle, wan_info, .. } = infra;

    // Delete the fwmark routing table rule
    if let Ok(rules) = handle.rule().get(IpVersion::V4).execute().try_collect::<Vec<_>>().await {
        for rule in rules.into_iter().filter(|rule| {
            rule.attributes
                .iter()
                .any(|a| matches!(a, RuleAttribute::FwMark(fwmark) if fwmark == &FW_MARK))
                && rule
                    .attributes
                    .iter()
                    .any(|a| matches!(a, RuleAttribute::Table(table) if table == &TABLE_ID))
        }) {
            if let Err(error) = handle.rule().del(rule).execute().await {
                tracing::warn!(%error, "failed to delete fwmark routing table rule, continuing anyway");
            } else {
                tracing::debug!("ip rule del mark {FW_MARK} table {TABLE_ID}");
            }
        }
    }

    // Delete the TABLE_ID routing table default route
    teardown_op(
        &format!("delete table {TABLE_ID} default route"),
        &format!(
            "ip route del default via {} dev {} table {TABLE_ID}",
            wan_info.gateway, wan_info.if_index
        ),
        || {
            handle
                .route()
                .del(default_route(wan_info.if_index, Some(wan_info.gateway), Some(TABLE_ID)))
                .execute()
        },
    )
    .await;

    // Flush routing cache
    teardown_op("flush routing cache", "ip route flush cache", flush_routing_cache).await;

    // Remove the iptables mangle and NAT rules
    teardown_op("teardown iptables rules", "iptables rules removed", || async {
        teardown_iptables(&wan_info.if_name, wan_info.ip_addr).map_err(Error::iptables)
    })
    .await;

    tracing::info!("fwmark infrastructure teardown complete");
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

        // Get interface's IPv4 address for SNAT
        let addresses: Vec<_> = handle.address().get().execute().try_collect().await?;
        let ip_addr = addresses
            .iter()
            .find_map(|addr| {
                if addr.header.index == if_index {
                    addr.attributes.iter().find_map(|attr| match attr {
                        AddressAttribute::Address(IpAddr::V4(ip)) => Some(*ip),
                        _ => None,
                    })
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::General(format!("WAN interface IP not found for index {if_index}")))?;

        Ok(WanInfo {
            if_index,
            if_name,
            gateway,
            ip_addr,
        })
    }

    /// Get VPN interface info via rtnetlink.
    /// Must be called after `wg-quick up` creates the VPN interface.
    async fn get_vpn_info_via_rtnetlink(
        handle: &rtnetlink::Handle,
        vpn_ip: &str,
        vpn_prefix: u8,
    ) -> Result<VpnInfo, Error> {
        let vpn_client_ip_cidr: cidr::Ipv4Cidr = cidr::parsers::parse_cidr_ignore_hostbits(vpn_ip, Ipv4Addr::from_str)
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

/// Dynamic router using rtnetlink and iptables for split-tunnel routing.
///
/// Uses firewall marks (fwmark) and policy-based routing to ensure HOPR traffic
/// bypasses the VPN while all other traffic routes through it.
///
/// This router assumes fwmark infrastructure is already set up via
/// `setup_fwmark_infrastructure`. It only handles VPN-specific routing:
/// - wg-quick up/down
/// - VPN subnet routes
/// - Default route through VPN
pub struct Router {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    // Once dropped, the spawned rtnetlink task will terminate
    handle: rtnetlink::Handle,
    /// WAN interface info, obtained from FwmarkInfrastructure
    wan_info: WanInfo,
    if_indices: Option<NetworkDeviceInfo>,
}

/// Static fallback router using direct `ip route` commands.
///
/// Used when dynamic routing (rtnetlink + iptables) is not available or not desired.
/// Simpler than [`Router`] but provides the same phased setup to avoid race conditions.
pub struct FallbackRouter {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    bypass_manager: Option<super::BypassRouteManager>,
}

// FwMark for traffic the does not go through the VPN
const FW_MARK: u32 = 0xFEED_CAFE;

// Table for traffic that does not go through the VPN
const TABLE_ID: u32 = 108;

// Priority of the FwMark routing table rule
const RULE_PRIORITY: u32 = 1;

const IP_TABLE: &str = "mangle";
const IP_CHAIN: &str = "OUTPUT";

const NAT_TABLE: &str = "nat";
const NAT_CHAIN: &str = "POSTROUTING";

// ============================================================================
// Route Message Builders
// ============================================================================
// Helper functions to reduce RouteMessageBuilder boilerplate throughout the router.

/// Builds a route message for a VPN subnet.
///
/// # Arguments
/// * `vpn_cidr` - The VPN subnet CIDR to route
/// * `vpn_if_index` - The VPN interface index
/// * `table_id` - Optional routing table ID (None = main table)
fn vpn_subnet_route(
    vpn_cidr: &cidr::Ipv4Cidr,
    vpn_if_index: u32,
    table_id: Option<u32>,
) -> rtnetlink::packet_route::route::RouteMessage {
    let mut builder = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
        .destination_prefix(vpn_cidr.first_address(), vpn_cidr.network_length())
        .output_interface(vpn_if_index);
    if let Some(id) = table_id {
        builder = builder.table_id(id);
    }
    builder.build()
}

/// Builds a default route message (0.0.0.0/0).
///
/// # Arguments
/// * `if_index` - The interface index for the route
/// * `gateway` - Optional gateway IP (None for interface routes)
/// * `table_id` - Optional routing table ID (None = main table)
fn default_route(
    if_index: u32,
    gateway: Option<Ipv4Addr>,
    table_id: Option<u32>,
) -> rtnetlink::packet_route::route::RouteMessage {
    let mut builder = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
        .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
        .output_interface(if_index);
    if let Some(gw) = gateway {
        builder = builder.gateway(gw);
    }
    if let Some(id) = table_id {
        builder = builder.table_id(id);
    }
    builder.build()
}

// ============================================================================
// Teardown Helpers
// ============================================================================
// Helpers to reduce repetitive "try-and-warn" patterns in teardown code.

/// Executes a teardown operation, logging warnings on failure but continuing.
///
/// This pattern is common in teardown code where we want to attempt cleanup
/// but not fail the entire teardown if one step fails.
///
/// # Arguments
/// * `op_name` - Description for the warning message (e.g., "delete VPN route")
/// * `debug_msg` - Message to log on success (e.g., "ip route del ...")
/// * `op` - The async operation to execute
async fn teardown_op<F, Fut, E>(op_name: &str, debug_msg: &str, op: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    if let Err(error) = op().await {
        tracing::warn!(%error, "failed to {}, continuing anyway", op_name);
    } else {
        tracing::debug!("{}", debug_msg);
    }
}

/// Sets up iptables rules for HOPR traffic bypass.
///
/// Marks ALL packets from the HOPR UID and uses SNAT for stable UDP/QUIC flows:
/// 1. `iptables -t mangle -A OUTPUT -o lo -j RETURN`
/// 2. `iptables -t mangle -A OUTPUT -m owner --uid-owner $VPN_UID -j MARK --set-mark $FW_MARK`
/// 3. `iptables -t nat -A POSTROUTING -m mark --mark $FW_MARK -o $WAN_IF -j SNAT --to-source $WAN_IP`
///
/// This approach marks every packet from the HOPR UID, ensuring reliable routing
/// even when the default route changes (VPN connects/disconnects). SNAT with a
/// fixed source IP is more stable for long-lived UDP flows than MASQUERADE.
fn setup_iptables(vpn_uid: u32, wan_if_name: &str, wan_ip: Ipv4Addr) -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    if iptables.chain_exists(IP_TABLE, IP_CHAIN)? {
        iptables.flush_chain(IP_TABLE, IP_CHAIN)?;
    } else {
        iptables.new_chain(IP_TABLE, IP_CHAIN)?;
    }

    // Rule 1: Keep loopback traffic unmarked
    iptables.append(IP_TABLE, IP_CHAIN, "-o lo -j RETURN")?;

    // Rule 2: Mark ALL traffic from VPN user (no conntrack dependency)
    iptables.append(
        IP_TABLE,
        IP_CHAIN,
        &format!("-m owner --uid-owner {vpn_uid} -j MARK --set-mark {FW_MARK}"),
    )?;

    // Rewrite the source address of bypassed (marked) traffic leaving via the WAN interface.
    // Use SNAT with fixed IP instead of MASQUERADE for more stable UDP/QUIC flows.
    // Without this, packets retain the VPN subnet source IP and the upstream gateway drops them
    // because it has no return route for that subnet.
    let nat_rule = format!("-m mark --mark {FW_MARK} -o {wan_if_name} -j SNAT --to-source {wan_ip}");
    // Delete any stale rule first (e.g. left over from a previous crash) to avoid duplicates.
    // Unlike the mangle chain we cannot flush nat POSTROUTING because other services use it.
    if iptables.exists(NAT_TABLE, NAT_CHAIN, &nat_rule)? {
        iptables.delete(NAT_TABLE, NAT_CHAIN, &nat_rule)?;
    }
    iptables.append(NAT_TABLE, NAT_CHAIN, &nat_rule)?;

    Ok(())
}

fn teardown_iptables(wan_if_name: &str, wan_ip: Ipv4Addr) -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    iptables.flush_chain(IP_TABLE, IP_CHAIN)?;

    // Delete only our specific NAT rule rather than flushing the entire nat POSTROUTING chain,
    // because other services (Docker, libvirt, etc.) install their own rules there.
    let nat_rule = format!("-m mark --mark {FW_MARK} -o {wan_if_name} -j SNAT --to-source {wan_ip}");
    if iptables.exists(NAT_TABLE, NAT_CHAIN, &nat_rule)? {
        iptables.delete(NAT_TABLE, NAT_CHAIN, &nat_rule)?;
    }

    Ok(())
}

/// Linux-specific implementation of [`Routing`] for split-tunnel routing.
#[async_trait]
impl Routing for Router {
    /// Install VPN-specific routing (assumes fwmark infrastructure is already set up).
    ///
    /// This method requires that fwmark infrastructure has been established via
    /// `setup_fwmark_infrastructure`. It only handles VPN-specific routing:
    ///
    /// Phase 1 (wg-quick up):
    ///   1. Generate wg-quick config and run wg-quick up
    ///
    /// Phase 2 (after wg-quick up):
    ///   2. Get VPN interface info
    ///   3. Add VPN subnet route to TABLE_ID (so bypassed traffic can reach VPN peers)
    ///   4. Add VPN subnet route to main table (overrides RFC1918 bypass for VPN server)
    ///   5. Flush routing cache (clear stale cached routes before default route change)
    ///   6. Replace main default route to VPN interface (atomic replace)
    ///   7. Flush routing cache (ensure all traffic uses new routes)
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.if_indices.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Use pre-existing WAN info from FwmarkInfrastructure
        let wan_info = &self.wan_info;
        tracing::debug!(?wan_info, "using WAN interface from fwmark infrastructure");

        // Phase 1: Bring up WireGuard
        // HOPR traffic is already protected by the fwmark infrastructure.

        // Step 1: Generate wg-quick config and run wg-quick up
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            vec!["Table = off".to_string()],
        );
        if let Err(e) = wg_tooling::up(self.state_home.clone(), wg_quick_content).await {
            return Err(e.into());
        }
        tracing::debug!("wg-quick up");

        // Phase 2: Complete routing with VPN interface info

        // Step 2: Get VPN interface info
        let vpn_info = match NetworkDeviceInfo::get_vpn_info_via_rtnetlink(
            &self.handle,
            &self.wg_data.interface_info.address,
            VPN_TUNNEL_SUBNET.1,
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                // Rollback: bring down WG
                if let Err(rollback_err) = wg_tooling::down(self.state_home.clone(), Logs::Suppress).await {
                    tracing::warn!(%rollback_err, "rollback failed: could not bring down WireGuard");
                }
                return Err(e);
            }
        };
        tracing::debug!(?vpn_info, "VPN interface data");

        // Store combined info for teardown
        self.if_indices = Some(NetworkDeviceInfo::from_parts(wan_info.clone(), vpn_info.clone()));

        // Step 3: Add VPN subnet route to TABLE_ID
        // This allows bypassed traffic to still reach VPN addresses
        let vpn_addrs_route = vpn_subnet_route(&vpn_info.cidr, vpn_info.if_index, Some(TABLE_ID));
        if let Err(e) = self.handle.route().add(vpn_addrs_route).execute().await {
            // Rollback: bring down WG
            self.if_indices = None;
            if let Err(rollback_err) = wg_tooling::down(self.state_home.clone(), Logs::Suppress).await {
                tracing::warn!(%rollback_err, "rollback failed: could not bring down WireGuard");
            }
            return Err(e.into());
        }
        tracing::debug!(
            "ip route add {} dev {} table {TABLE_ID}",
            vpn_info.cidr,
            vpn_info.if_index
        );

        // Step 4: Add VPN subnet route to main table
        // This ensures VPN subnet traffic uses tunnel, overriding any pre-existing RFC1918 routes
        let vpn_subnet_main_route = vpn_subnet_route(&vpn_info.cidr, vpn_info.if_index, None);
        if let Err(e) = self.handle.route().add(vpn_subnet_main_route).execute().await {
            // Log warning but continue - default route should still work
            tracing::warn!(%e, "failed to add VPN subnet route to main table");
        }
        tracing::debug!("ip route add {} dev {}", vpn_info.cidr, vpn_info.if_index);

        // Step 5: Flush routing cache BEFORE changing default route
        // This ensures any cached route decisions are cleared before the switch
        flush_routing_cache().await?;
        tracing::debug!("flushed routing cache before default route change");

        // Step 6: Replace main default route to VPN interface
        // All non-bypassed traffic now goes through VPN
        // Use replace() for atomic replacement - avoids brief window with two default routes
        let vpn_default_route = default_route(vpn_info.if_index, None, None);
        if let Err(e) = self.handle.route().add(vpn_default_route).replace().execute().await {
            // Rollback: remove VPN subnet route, bring down WG
            if let Err(rollback_err) = self
                .handle
                .route()
                .del(vpn_subnet_route(&vpn_info.cidr, vpn_info.if_index, Some(TABLE_ID)))
                .execute()
                .await
            {
                tracing::warn!(%rollback_err, "rollback failed: could not delete VPN subnet route from TABLE_ID");
            }
            self.if_indices = None;
            if let Err(rollback_err) = wg_tooling::down(self.state_home.clone(), Logs::Suppress).await {
                tracing::warn!(%rollback_err, "rollback failed: could not bring down WireGuard");
            }
            return Err(e.into());
        }
        tracing::debug!("ip route add default dev {}", vpn_info.if_index);

        // Step 7: Flush routing cache
        flush_routing_cache().await?;
        tracing::debug!("flushed routing cache");

        tracing::info!("routing is ready");
        Ok(())
    }

    /// Uninstalls VPN-specific routing (fwmark infrastructure remains active).
    ///
    /// This method only tears down VPN-specific routes and the WireGuard interface.
    /// The fwmark infrastructure (iptables rules, TABLE_ID default route, fwmark rule)
    /// remains active for the daemon's lifetime.
    ///
    /// The steps:
    ///   1. Restore the default route in the MAIN routing table to WAN (atomic replace)
    ///      Equivalent command: `ip route replace default via $WAN_GW dev $IF_WAN`
    ///   2. Delete the VPN subnet route from the MAIN table
    ///      Equivalent command: `ip route del $VPN_SUBNET dev $IF_VPN`
    ///   3. Run `wg-quick down` (while bypass is still active for HOPR traffic)
    ///   4. Delete the VPN subnet route from TABLE_ID
    ///      Equivalent command: `ip route del $VPN_SUBNET dev $IF_VPN table $TABLE_ID`
    ///   5. Flush the routing table cache
    ///      Equivalent command: `ip route flush cache`
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        let NetworkDeviceInfo {
            wan_if_index,
            wan_if_name: _,
            vpn_if_index,
            vpn_gw: _,
            vpn_cidr,
            wan_gw,
        } = self
            .if_indices
            .take()
            .ok_or(Error::General("invalid state: not set up".into()))?;

        // Step 1: Set the default route back to the WAN interface
        // Use replace() to handle case where VPN route still exists
        teardown_op(
            "set default route back to interface",
            &format!("ip route replace default via {wan_gw} dev {wan_if_index}"),
            || {
                self.handle
                    .route()
                    .add(default_route(wan_if_index, Some(wan_gw), None))
                    .replace()
                    .execute()
            },
        )
        .await;

        // Step 2: Delete the VPN subnet route from the main table
        teardown_op(
            "delete VPN subnet route from main table",
            &format!("ip route del {vpn_cidr} dev {vpn_if_index}"),
            || {
                self.handle
                    .route()
                    .del(vpn_subnet_route(&vpn_cidr, vpn_if_index, None))
                    .execute()
            },
        )
        .await;

        // Step 3: Run wg-quick down while bypass infrastructure is still active
        // HOPR traffic continues: iptables marks → fwmark rule → TABLE_ID → WAN
        wg_tooling::down(self.state_home.clone(), logs).await?;
        tracing::debug!("wg-quick down");

        // Step 4: Delete the TABLE_ID routing table VPN route
        // (fwmark rule and TABLE_ID default route stay active for the daemon's lifetime)
        teardown_op(
            &format!("delete VPN subnet route from table {TABLE_ID}"),
            &format!("ip route del {vpn_cidr} dev {vpn_if_index} table {TABLE_ID}"),
            || {
                self.handle
                    .route()
                    .del(vpn_subnet_route(&vpn_cidr, vpn_if_index, Some(TABLE_ID)))
                    .execute()
            },
        )
        .await;

        // Step 5: Flush routing cache
        flush_routing_cache().await?;
        tracing::debug!("ip route flush cache");

        tracing::info!("VPN routing teardown complete (fwmark infrastructure remains active)");
        Ok(())
    }
}

#[async_trait]
impl Routing for FallbackRouter {
    /// Install split-tunnel routing for FallbackRouter.
    ///
    /// Uses a phased approach to avoid a race condition where HOPR p2p connections
    /// could briefly drop when the WireGuard interface comes up.
    ///
    /// Phase 1 (before wg-quick up):
    ///   1. Get WAN interface info
    ///   2. Add bypass routes for all peer IPs directly via WAN
    ///   3. Add RFC1918 bypass routes (10.0.0.0/8, etc.) via WAN for LAN access
    ///
    /// Phase 2:
    ///   4. Run wg-quick up with PostUp hook for VPN subnet route (10.128.0.0/9)
    ///      The VPN subnet route overrides the 10.0.0.0/8 bypass for VPN server traffic
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.bypass_manager.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Phase 1: Add bypass routes BEFORE wg-quick up
        let (device, gateway) = interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut bypass_manager =
            super::BypassRouteManager::new(super::WanInterface { device, gateway }, self.peer_ips.clone());

        // Add peer IP and RFC1918 bypass routes (auto-rollback on failure)
        bypass_manager.setup_peer_routes().await?;
        bypass_manager.setup_rfc1918_routes().await?;

        // Phase 2: wg-quick up with PostUp for VPN subnet route
        let extra = vec![
            "Table = off".to_string(),
            // VPN internal subnet (more specific than 10.0.0.0/8 bypass)
            format!(
                "PostUp = ip route add {}/{} dev %i",
                VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1
            ),
        ];
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        if let Err(e) = wg_tooling::up(self.state_home.clone(), wg_quick_content).await {
            tracing::warn!("wg-quick up failed, rolling back bypass routes");
            bypass_manager.rollback().await;
            return Err(e.into());
        }
        tracing::debug!("wg-quick up");

        self.bypass_manager = Some(bypass_manager);
        tracing::info!("routing is ready (fallback)");
        Ok(())
    }

    /// Teardown split-tunnel routing for FallbackRouter.
    ///
    /// Teardown order is important: wg-quick down first, then remove bypass routes.
    /// This ensures HOPR traffic continues to flow via WAN while VPN is being torn down.
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        // wg-quick down first
        wg_tooling::down(self.state_home.clone(), logs).await?;
        tracing::debug!("wg-quick down");

        // then remove bypass routes
        if let Some(ref mut bypass_manager) = self.bypass_manager {
            bypass_manager.teardown().await;
        }
        self.bypass_manager = None;

        Ok(())
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

/// Gets the default WAN interface name and gateway by querying the routing table.
///
/// Returns `(device_name, Option<gateway_ip>)`.
/// Used by FallbackRouter; the dynamic Router uses rtnetlink directly.
async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("ip")
        .arg("route")
        .arg("show")
        .arg("default")
        .run_stdout(Logs::Print)
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

/// Parses the output of `ip route show default` to extract interface and gateway.
fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    super::parse_key_value_output(output, "dev", "via", None)
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
