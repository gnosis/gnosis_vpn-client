//! Linux routing implementation for split-tunnel VPN behavior.
//!
//! Provides two router implementations:
//!
//! ## [`Router`] (Dynamic)
//! Uses rtnetlink and firewall rules for advanced split-tunnel routing:
//! 1. Sets up firewall rules to mark HOPR traffic with a firewall mark (fwmark)
//! 2. Creates a separate routing table (TABLE_ID) for marked traffic to bypass VPN
//! 3. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 4. Adds VPN subnet route (10.128.0.0/9) to both TABLE_ID and main table
//! 5. Configures default route through VPN for all other traffic
//!
//! ## [`FallbackRouter`] (Static)
//! Simpler implementation using route operations via netlink:
//! 1. Adds bypass routes for peer IPs BEFORE bringing up WireGuard (avoids race condition)
//! 2. Adds RFC1918 bypass routes (10.0.0.0/8, etc.) via WAN for LAN access
//! 3. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 4. Adds VPN subnet route (10.128.0.0/9) programmatically after wg-quick up
//! 5. On teardown, removes VPN subnet route, brings down WireGuard, then cleans up bypass routes
//!
//! Both implementations use a phased approach to avoid race conditions during VPN setup.
//!
//! ## Route Precedence
//! The VPN subnet (10.128.0.0/9) is more specific than the RFC1918 bypass (10.0.0.0/8),
//! so VPN server traffic (e.g. to 10.128.0.1) routes through the tunnel while other
//! RFC1918 Class A traffic bypasses to the WAN for LAN access.

use async_trait::async_trait;

use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::{event, wireguard, worker};

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use super::netlink_ops::{NetlinkOps, RealNetlinkOps, RouteSpec, RuleSpec};
use super::nftables_ops::{self, NfTablesOps, RealNfTablesOps};
use super::route_ops::RouteOps;
use super::route_ops_linux::NetlinkRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, Routing, VPN_TUNNEL_SUBNET};

// ============================================================================
// Type Aliases for Production Use
// ============================================================================

/// Production fwmark infrastructure using real netlink.
pub type FwmarkInfra = FwmarkInfrastructure<RealNetlinkOps>;

// ============================================================================
// Factory Functions (public convenience wrappers)
// ============================================================================

/// Creates a dynamic router using rtnetlink and firewall rules.
///
/// This is the preferred router on Linux as it provides more robust split-tunnel
/// routing using firewall marks (fwmark) and policy-based routing.
///
/// The router requires pre-existing fwmark infrastructure (set up via
/// `setup_fwmark_infrastructure`) and only handles VPN-specific routing.
pub fn dynamic_router(
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    wan_info: WanInfo,
    handle: rtnetlink::Handle,
) -> Result<impl Routing, Error> {
    let netlink = RealNetlinkOps::new(handle.clone());
    let route_ops = NetlinkRouteOps::new(handle);
    let wg = RealWgOps;
    Ok(Router {
        state_home,
        wg_data,
        netlink,
        route_ops,
        wg,
        wan_info,
        if_indices: None,
    })
}

/// Creates a static fallback router using route operations via netlink.
///
/// Used when dynamic routing is not available. Provides simpler routing
/// by adding explicit host routes for peer IPs before bringing up WireGuard.
pub fn static_fallback_router(
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle);
    let wg = RealWgOps;
    Ok(FallbackRouter {
        state_home,
        wg_data,
        peer_ips,
        route_ops,
        wg,
        bypass_manager: None,
        vpn_subnet_route_added: false,
    })
}

// ============================================================================
// Fwmark Infrastructure (public convenience wrappers)
// ============================================================================

/// Cleans up any stale firewall rules and routing entries from a previous crash.
///
/// This should be called at daemon startup before `setup_fwmark_infrastructure()`
/// to ensure a clean slate. Safe to call even if no stale rules exist.
///
/// Cleanup is best-effort: errors are logged but do not prevent startup.
pub async fn cleanup_stale_fwmark_rules() {
    tracing::debug!("checking for stale fwmark infrastructure from previous crash");

    let nft = match RealNfTablesOps::new() {
        Ok(nft) => nft,
        Err(e) => {
            tracing::debug!("cannot create firewall ops for stale cleanup: {e}");
            return;
        }
    };

    let (conn, handle, _) = match rtnetlink::new_connection() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("cannot create rtnetlink for stale cleanup: {e}");
            return;
        }
    };
    tokio::task::spawn(conn);
    let netlink = RealNetlinkOps::new(handle);

    cleanup_stale_fwmark_rules_with(&netlink, &nft).await;
}

/// Sets up the persistent fwmark infrastructure at daemon startup.
///
/// This establishes the firewall rules and routing table entries that
/// allow HOPR traffic to bypass the VPN tunnel. This setup persists
/// for the lifetime of the daemon, independent of individual VPN connections.
///
/// The setup includes:
/// 1. Creating an rtnetlink connection for route management
/// 2. Getting WAN interface info (index, name, gateway)
/// 3. Setting up firewall rules to mark HOPR traffic with FW_MARK
/// 4. Creating TABLE_ID routing table with WAN as default gateway
/// 5. Adding fwmark rule: marked traffic uses TABLE_ID (bypasses VPN)
pub async fn setup_fwmark_infrastructure(worker: &worker::Worker) -> Result<FwmarkInfra, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let netlink = RealNetlinkOps::new(handle);
    let nft = RealNfTablesOps::new()?;
    setup_fwmark_infrastructure_with(worker, netlink, &nft).await
}

/// Tears down the persistent fwmark infrastructure at daemon shutdown.
///
/// This removes the firewall rules and routing table entries that were
/// set up by `setup_fwmark_infrastructure`.
///
/// The teardown includes:
/// 1. Deleting the fwmark routing rule
/// 2. Deleting the TABLE_ID default route
/// 3. Removing firewall mangle and NAT rules
pub async fn teardown_fwmark_infrastructure(infra: FwmarkInfra) {
    let nft = match RealNfTablesOps::new() {
        Ok(nft) => nft,
        Err(e) => {
            tracing::warn!("cannot create firewall ops for teardown, cleanup will happen at next startup: {e}");
            return;
        }
    };
    teardown_fwmark_infrastructure_with(infra, &nft).await;
}

// ============================================================================
// Generic (testable) implementations
// ============================================================================

/// Testable version of `cleanup_stale_fwmark_rules`.
pub(crate) async fn cleanup_stale_fwmark_rules_with<N: NetlinkOps, F: NfTablesOps>(netlink: &N, nft: &F) {
    // Try to remove stale firewall rules (ignore errors - they may not exist)
    if let Err(e) = nft.cleanup_stale_rules(FW_MARK) {
        tracing::debug!("stale firewall cleanup error (may be benign): {e}");
    }

    // Try to remove stale TABLE_ID routes and fwmark rules via netlink

    // Delete fwmark rule if exists
    if let Ok(rules) = netlink.rule_list_v4().await {
        for rule in &rules {
            if rule.fw_mark == FW_MARK || rule.table_id == TABLE_ID {
                tracing::info!("found stale fwmark rule - cleaning up");
                let _ = netlink.rule_del(rule).await;
                break;
            }
        }
    }

    // Delete TABLE_ID routes
    if let Ok(routes) = netlink.route_list(Some(TABLE_ID)).await {
        for route in &routes {
            tracing::info!("found stale route in table {} - cleaning up", TABLE_ID);
            let _ = netlink.route_del(route).await;
        }
    }

    tracing::debug!("stale fwmark infrastructure cleanup complete");
}

/// Testable version of `setup_fwmark_infrastructure`.
pub(crate) async fn setup_fwmark_infrastructure_with<N: NetlinkOps, F: NfTablesOps>(
    worker: &worker::Worker,
    netlink: N,
    nft: &F,
) -> Result<FwmarkInfrastructure<N>, Error> {
    // Get WAN interface info
    let wan_info = get_wan_info(&netlink).await?;
    tracing::debug!(?wan_info, "WAN interface data for fwmark infrastructure");

    // Setup firewall rules to mark HOPR traffic for bypass
    nft.setup_fwmark_rules(worker.uid, &wan_info.if_name, FW_MARK, wan_info.ip_addr)?;
    tracing::debug!(uid = worker.uid, wan_ip = %wan_info.ip_addr, "firewall rules set up");

    // Create TABLE_ID with WAN default route
    let no_vpn_route = RouteSpec {
        destination: Ipv4Addr::UNSPECIFIED,
        prefix_len: 0,
        gateway: Some(wan_info.gateway),
        if_index: wan_info.if_index,
        table_id: Some(TABLE_ID),
    };
    if let Err(e) = netlink.route_add(&no_vpn_route).await {
        // Rollback firewall rules on failure
        if let Err(rollback_err) = nft.teardown_rules(&wan_info.if_name, FW_MARK, wan_info.ip_addr) {
            tracing::warn!(%rollback_err, "rollback failed: could not teardown firewall rules");
        }
        return Err(e);
    }
    tracing::debug!(
        "ip route add default via {} dev {} table {TABLE_ID}",
        wan_info.gateway,
        wan_info.if_index
    );

    // Add fwmark rule - marked traffic goes to TABLE_ID
    let fwmark_rule = RuleSpec {
        fw_mark: FW_MARK,
        table_id: TABLE_ID,
        priority: RULE_PRIORITY,
    };
    if let Err(e) = netlink.rule_add(&fwmark_rule).await {
        // Rollback TABLE_ID route and firewall rules on failure
        if let Err(rollback_err) = netlink.route_del(&no_vpn_route).await {
            tracing::warn!(%rollback_err, "rollback failed: could not delete TABLE_ID default route");
        }
        if let Err(rollback_err) = nft.teardown_rules(&wan_info.if_name, FW_MARK, wan_info.ip_addr) {
            tracing::warn!(%rollback_err, "rollback failed: could not teardown firewall rules");
        }
        return Err(e);
    }
    tracing::debug!("ip rule add mark {FW_MARK} table {TABLE_ID} pref {RULE_PRIORITY}");

    tracing::info!("fwmark infrastructure is ready");
    Ok(FwmarkInfrastructure {
        netlink,
        wan_info,
        torn_down: false,
    })
}

/// Testable version of `teardown_fwmark_infrastructure`.
pub(crate) async fn teardown_fwmark_infrastructure_with<N: NetlinkOps, F: NfTablesOps>(
    mut infra: FwmarkInfrastructure<N>,
    nft: &F,
) {
    // Mark as torn down before we start cleanup - this prevents the Drop warning
    infra.torn_down = true;
    let netlink = &infra.netlink;
    let wan_info = &infra.wan_info;

    // Delete the fwmark routing table rule
    match netlink.rule_list_v4().await {
        Ok(rules) => {
            for rule in rules.iter().filter(|r| r.fw_mark == FW_MARK && r.table_id == TABLE_ID) {
                if let Err(error) = netlink.rule_del(rule).await {
                    tracing::warn!(%error, "failed to delete fwmark routing table rule, continuing anyway");
                } else {
                    tracing::debug!("ip rule del mark {FW_MARK} table {TABLE_ID}");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to list rules for cleanup, continuing anyway");
        }
    }

    // Delete the TABLE_ID routing table default route
    let table_route = RouteSpec {
        destination: Ipv4Addr::UNSPECIFIED,
        prefix_len: 0,
        gateway: Some(wan_info.gateway),
        if_index: wan_info.if_index,
        table_id: Some(TABLE_ID),
    };
    teardown_op(
        &format!("delete table {TABLE_ID} default route"),
        &format!(
            "ip route del default via {} dev {} table {TABLE_ID}",
            wan_info.gateway, wan_info.if_index
        ),
        || netlink.route_del(&table_route),
    )
    .await;

    // Remove the firewall mangle and NAT rules
    teardown_op("teardown firewall rules", "firewall rules removed", || async {
        nft.teardown_rules(&wan_info.if_name, FW_MARK, wan_info.ip_addr)
    })
    .await;

    tracing::info!("fwmark infrastructure teardown complete");
}

// ============================================================================
// Structs
// ============================================================================

#[derive(Debug, Clone)]
struct NetworkDeviceInfo {
    /// Index of the WAN interface
    wan_if_index: u32,
    /// Default gateway of the WAN interface
    wan_gw: Ipv4Addr,
    /// Index of the VPN interface
    vpn_if_index: u32,
    /// CIDR of the VPN subnet
    vpn_cidr: cidr::Ipv4Cidr,
}

/// WAN interface information gathered before VPN interface exists.
///
/// **Limitation:** The `ip_addr` field is captured once at daemon startup and used for SNAT rules.
/// If the WAN IP changes during operation (DHCP renewal, network switch), the SNAT rules
/// will use the stale IP, causing bypassed traffic to fail silently. In such cases,
/// restarting the daemon will refresh the IP. Using MASQUERADE instead of SNAT would
/// handle IP changes automatically but may cause connection instability for long-lived
/// UDP/QUIC flows.
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
/// to clean up firewall rules and routing table entries. Dropping without teardown
/// will log a warning and may leave the system in an inconsistent state.
pub struct FwmarkInfrastructure<N: NetlinkOps> {
    pub netlink: N,
    pub wan_info: WanInfo,
    /// Tracks whether teardown was called. Set to true when teardown_fwmark_infrastructure() is invoked.
    pub(super) torn_down: bool,
}

impl<N: NetlinkOps> Drop for FwmarkInfrastructure<N> {
    fn drop(&mut self) {
        if !self.torn_down {
            tracing::warn!(
                "FwmarkInfrastructure dropped without teardown - firewall rules and routing entries may be leaked"
            );
        }
    }
}

/// VPN interface information gathered after `wg-quick up`.
#[derive(Debug, Clone)]
struct VpnInfo {
    if_index: u32,
    cidr: cidr::Ipv4Cidr,
}

impl NetworkDeviceInfo {
    /// Construct `NetworkDeviceInfo` from separately gathered WAN and VPN info.
    fn from_parts(wan: WanInfo, vpn: VpnInfo) -> Self {
        Self {
            wan_if_index: wan.if_index,
            wan_gw: wan.gateway,
            vpn_if_index: vpn.if_index,
            vpn_cidr: vpn.cidr,
        }
    }
}

/// Dynamic router using rtnetlink and firewall rules for split-tunnel routing.
///
/// Uses firewall marks (fwmark) and policy-based routing to ensure HOPR traffic
/// bypasses the VPN while all other traffic routes through it.
///
/// This router assumes fwmark infrastructure is already set up via
/// `setup_fwmark_infrastructure`. It only handles VPN-specific routing:
/// - wg-quick up/down
/// - VPN subnet routes
/// - Default route through VPN
pub struct Router<N: NetlinkOps, R: RouteOps, W: WgOps> {
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    netlink: N,
    route_ops: R,
    wg: W,
    /// WAN interface info, obtained from FwmarkInfrastructure
    wan_info: WanInfo,
    if_indices: Option<NetworkDeviceInfo>,
}

/// Static fallback router using route operations via netlink.
///
/// Used when dynamic routing (rtnetlink + firewall rules) is not available or not desired.
/// Simpler than [`Router`] but provides the same phased setup to avoid race conditions.
pub struct FallbackRouter<R: RouteOps, W: WgOps> {
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
    bypass_manager: Option<super::BypassRouteManager<R>>,
    /// Whether the VPN subnet route (10.128.0.0/9) was successfully added.
    vpn_subnet_route_added: bool,
}

// ============================================================================
// Constants
// ============================================================================

/// Firewall mark used to identify HOPR traffic for bypass routing.
///
/// This mark is applied by firewall rules to packets owned by the worker process (UID-based),
/// allowing policy-based routing to send them via WAN instead of VPN tunnel.
///
/// Value 0xFEED_CAFE is arbitrary but memorable and unlikely to conflict with
/// other fwmark users (Docker uses 0x1, etc.).
const FW_MARK: u32 = nftables_ops::FW_MARK;

/// Routing table ID for fwmark-based bypass traffic.
///
/// Policy routing rule directs FW_MARK-ed packets to this table, which contains
/// a default route via WAN gateway. Value 108 is arbitrary but avoids conflicts
/// with standard tables (local=255, main=254, default=253) and common custom
/// tables (Docker typically uses 100-107).
const TABLE_ID: u32 = 108;

/// Priority for the fwmark routing rule.
///
/// Lower values = higher priority. Value 1 ensures our bypass rule is evaluated
/// before most other policy rules while still allowing local table (priority 0)
/// to handle loopback traffic.
const RULE_PRIORITY: u32 = 1;

// ============================================================================
// Network Info Helpers
// ============================================================================

/// Get WAN interface info via netlink.
/// Can be called before VPN interface exists.
async fn get_wan_info<N: NetlinkOps>(netlink: &N) -> Result<WanInfo, Error> {
    // The default route is the one with the longest prefix match (= smallest prefix length)
    let routes = netlink.route_list(None).await?;
    let default_route = routes.iter().min_by_key(|r| r.prefix_len).ok_or(Error::NoInterface)?;

    let if_index = default_route.if_index;
    let gateway = default_route.gateway.ok_or(Error::NoInterface)?;

    let links = netlink.link_list().await?;
    let if_name = links
        .iter()
        .find(|l| l.index == if_index)
        .map(|l| l.name.clone())
        .ok_or_else(|| Error::General(format!("WAN interface name not found for index {if_index}")))?;

    // Get interface's IPv4 address for SNAT
    let addrs = netlink.addr_list_v4().await?;
    let ip_addr = addrs
        .iter()
        .find(|a| a.if_index == if_index)
        .map(|a| a.addr)
        .ok_or_else(|| Error::General(format!("WAN interface IP not found for index {if_index}")))?;

    Ok(WanInfo {
        if_index,
        if_name,
        gateway,
        ip_addr,
    })
}

/// Get VPN interface info via netlink.
/// Must be called after `wg-quick up` creates the VPN interface.
async fn get_vpn_info<N: NetlinkOps>(netlink: &N, vpn_ip: &str, vpn_prefix: u8) -> Result<VpnInfo, Error> {
    use std::str::FromStr;

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

    let links = netlink.link_list().await?;
    let if_index = links
        .iter()
        .find(|l| l.name == wireguard::WG_INTERFACE)
        .map(|l| l.index)
        .ok_or(Error::NoInterface)?;

    Ok(VpnInfo { if_index, cidr })
}

// ============================================================================
// Teardown Helpers
// ============================================================================

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

// ============================================================================
// Routing Implementations
// ============================================================================

/// Linux-specific implementation of [`Routing`] for split-tunnel routing.
#[async_trait]
impl<N: NetlinkOps + 'static, R: RouteOps + 'static, W: WgOps + 'static> Routing for Router<N, R, W> {
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
        self.wg
            .wg_quick_up((*self.state_home).clone(), wg_quick_content)
            .await?;
        tracing::debug!("wg-quick up");

        // Phase 2: Complete routing with VPN interface info

        // Step 2: Get VPN interface info
        let vpn_info =
            match get_vpn_info(&self.netlink, &self.wg_data.interface_info.address, VPN_TUNNEL_SUBNET.1).await {
                Ok(info) => info,
                Err(e) => {
                    // Rollback: bring down WG
                    if let Err(rollback_err) = self.wg.wg_quick_down((*self.state_home).clone(), Logs::Suppress).await {
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
        let vpn_table_route = RouteSpec {
            destination: vpn_info.cidr.first_address(),
            prefix_len: vpn_info.cidr.network_length(),
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: Some(TABLE_ID),
        };
        if let Err(e) = self.netlink.route_add(&vpn_table_route).await {
            // Rollback: bring down WG
            self.if_indices = None;
            if let Err(rollback_err) = self.wg.wg_quick_down((*self.state_home).clone(), Logs::Suppress).await {
                tracing::warn!(%rollback_err, "rollback failed: could not bring down WireGuard");
            }
            return Err(e);
        }
        tracing::debug!(
            "ip route add {} dev {} table {TABLE_ID}",
            vpn_info.cidr,
            vpn_info.if_index
        );

        // Step 4: Add VPN subnet route to main table
        // This ensures VPN subnet traffic uses tunnel, overriding any pre-existing RFC1918 routes
        let vpn_main_route = RouteSpec {
            destination: vpn_info.cidr.first_address(),
            prefix_len: vpn_info.cidr.network_length(),
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: None,
        };
        if let Err(e) = self.netlink.route_add(&vpn_main_route).await {
            // Log warning but continue - default route should still work
            tracing::warn!(%e, "failed to add VPN subnet route to main table");
        }
        tracing::debug!("ip route add {} dev {}", vpn_info.cidr, vpn_info.if_index);

        // Step 5: Flush routing cache BEFORE changing default route
        // This ensures any cached route decisions are cleared before the switch
        self.route_ops.flush_routing_cache().await?;
        tracing::debug!("flushed routing cache before default route change");

        // Step 6: Replace main default route to VPN interface
        // All non-bypassed traffic now goes through VPN
        // Use replace() for atomic replacement - avoids brief window with two default routes
        let vpn_default_route = RouteSpec {
            destination: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: None,
        };
        if let Err(e) = self.netlink.route_replace(&vpn_default_route).await {
            // Rollback: remove VPN subnet route, bring down WG
            if let Err(rollback_err) = self.netlink.route_del(&vpn_table_route).await {
                tracing::warn!(%rollback_err, "rollback failed: could not delete VPN subnet route from TABLE_ID");
            }
            self.if_indices = None;
            if let Err(rollback_err) = self.wg.wg_quick_down((*self.state_home).clone(), Logs::Suppress).await {
                tracing::warn!(%rollback_err, "rollback failed: could not bring down WireGuard");
            }
            return Err(e);
        }
        tracing::debug!("ip route add default dev {}", vpn_info.if_index);

        // Step 7: Flush routing cache
        self.route_ops.flush_routing_cache().await?;
        tracing::debug!("flushed routing cache");

        tracing::info!("routing is ready");
        Ok(())
    }

    /// Uninstalls VPN-specific routing (fwmark infrastructure remains active).
    ///
    /// This method only tears down VPN-specific routes and the WireGuard interface.
    /// The fwmark infrastructure (firewall rules, TABLE_ID default route, fwmark rule)
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
            vpn_if_index,
            vpn_cidr,
            wan_gw,
        } = self
            .if_indices
            .take()
            .ok_or(Error::General("invalid state: not set up".into()))?;

        // Step 1: Set the default route back to the WAN interface
        // Use replace() to handle case where VPN route still exists
        let wan_default = RouteSpec {
            destination: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
            gateway: Some(wan_gw),
            if_index: wan_if_index,
            table_id: None,
        };
        teardown_op(
            "set default route back to interface",
            &format!("ip route replace default via {wan_gw} dev {wan_if_index}"),
            || self.netlink.route_replace(&wan_default),
        )
        .await;

        // Step 2: Delete the VPN subnet route from the main table
        let vpn_main_route = RouteSpec {
            destination: vpn_cidr.first_address(),
            prefix_len: vpn_cidr.network_length(),
            gateway: None,
            if_index: vpn_if_index,
            table_id: None,
        };
        teardown_op(
            "delete VPN subnet route from main table",
            &format!("ip route del {vpn_cidr} dev {vpn_if_index}"),
            || self.netlink.route_del(&vpn_main_route),
        )
        .await;

        // Step 3: Run wg-quick down while bypass infrastructure is still active
        // HOPR traffic continues: firewall marks -> fwmark rule -> TABLE_ID -> WAN
        self.wg.wg_quick_down((*self.state_home).clone(), logs).await?;
        tracing::debug!("wg-quick down");

        // Step 4: Delete the TABLE_ID routing table VPN route
        // (fwmark rule and TABLE_ID default route stay active for the daemon's lifetime)
        let vpn_table_route = RouteSpec {
            destination: vpn_cidr.first_address(),
            prefix_len: vpn_cidr.network_length(),
            gateway: None,
            if_index: vpn_if_index,
            table_id: Some(TABLE_ID),
        };
        teardown_op(
            &format!("delete VPN subnet route from table {TABLE_ID}"),
            &format!("ip route del {vpn_cidr} dev {vpn_if_index} table {TABLE_ID}"),
            || self.netlink.route_del(&vpn_table_route),
        )
        .await;

        // Step 5: Flush routing cache
        self.route_ops.flush_routing_cache().await?;
        tracing::debug!("ip route flush cache");

        tracing::info!("VPN routing teardown complete (fwmark infrastructure remains active)");
        Ok(())
    }
}

#[async_trait]
impl<R: RouteOps + 'static, W: WgOps + 'static> Routing for FallbackRouter<R, W> {
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
    ///   4. Run wg-quick up with Table = off (no automatic routing)
    ///
    /// Phase 3 (after wg-quick up):
    ///   5. Add VPN subnet route (10.128.0.0/9) programmatically
    ///      The VPN subnet route overrides the 10.0.0.0/8 bypass for VPN server traffic
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.bypass_manager.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Phase 1: Add bypass routes BEFORE wg-quick up
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut bypass_manager = super::BypassRouteManager::new(
            super::WanInterface { device, gateway },
            self.peer_ips.clone(),
            self.route_ops.clone(),
        );

        // Add peer IP and RFC1918 bypass routes (auto-rollback on failure)
        bypass_manager.setup_peer_routes().await?;
        bypass_manager.setup_rfc1918_routes().await?;

        // Phase 2: wg-quick up with Table = off only (no PostUp hooks)
        let extra = vec!["Table = off".to_string()];
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        if let Err(e) = self.wg.wg_quick_up((*self.state_home).clone(), wg_quick_content).await {
            tracing::warn!("wg-quick up failed, rolling back bypass routes");
            bypass_manager.rollback().await;
            return Err(e);
        }
        tracing::debug!("wg-quick up");

        // Phase 3: Add VPN subnet route programmatically
        let vpn_subnet = format!("{}/{}", VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1);
        if let Err(e) = self
            .route_ops
            .route_add(&vpn_subnet, None, wireguard::WG_INTERFACE)
            .await
        {
            tracing::warn!(%e, "VPN subnet route failed, rolling back");
            if let Err(wg_err) = self.wg.wg_quick_down((*self.state_home).clone(), Logs::Suppress).await {
                tracing::warn!(%wg_err, "rollback failed: could not bring down WireGuard");
            }
            bypass_manager.rollback().await;
            return Err(e);
        }
        self.vpn_subnet_route_added = true;
        tracing::debug!(subnet = %vpn_subnet, "VPN subnet route added");

        self.bypass_manager = Some(bypass_manager);
        tracing::info!("routing is ready (fallback)");
        Ok(())
    }

    /// Teardown split-tunnel routing for FallbackRouter.
    ///
    /// Teardown order:
    /// 1. Remove VPN subnet route (best-effort)
    /// 2. wg-quick down
    /// 3. Remove bypass routes
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        // Remove VPN subnet route (best-effort)
        if self.vpn_subnet_route_added {
            let vpn_subnet = format!("{}/{}", VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1);
            if let Err(e) = self.route_ops.route_del(&vpn_subnet, wireguard::WG_INTERFACE).await {
                tracing::warn!(%e, "failed to remove VPN subnet route during teardown");
            }
            self.vpn_subnet_route_added = false;
        }

        // wg-quick down
        let wg_result = self.wg.wg_quick_down((*self.state_home).clone(), logs).await;
        if let Err(ref e) = wg_result {
            tracing::warn!(%e, "wg-quick down failed, continuing with bypass route cleanup");
        } else {
            tracing::debug!("wg-quick down");
        }

        // Remove bypass routes (always, even if wg-quick down failed)
        if let Some(ref mut bypass_manager) = self.bypass_manager {
            bypass_manager.teardown().await;
        }
        self.bypass_manager = None;

        wg_result
    }
}

/// Parses the output of `ip route show default` to extract interface and gateway.
#[cfg(test)]
fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    super::parse_key_value_output(output, "dev", "via", None)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    use super::*;
    use crate::routing::mocks::*;
    use crate::routing::netlink_ops::{AddrInfo, LinkInfo};

    // ====================================================================
    // Parse tests (preserved from original)
    // ====================================================================

    #[test]
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = "default via 192.168.101.1 dev wlp2s0 proto dhcp src 192.168.101.202 metric 600 ";

        let (device, gateway) = parse_interface(output)?;

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

    // ====================================================================
    // Helper functions for tests
    // ====================================================================

    fn mock_worker() -> worker::Worker {
        worker::Worker {
            uid: 1000,
            gid: 1000,
            group_name: "test".into(),
            binary: "/bin/test".into(),
            home: PathBuf::from("/tmp/test"),
        }
    }

    fn wan_info() -> WanInfo {
        WanInfo {
            if_index: 2,
            if_name: "eth0".into(),
            gateway: Ipv4Addr::new(192, 168, 1, 1),
            ip_addr: Ipv4Addr::new(192, 168, 1, 100),
        }
    }

    fn mock_netlink_with_wan() -> MockNetlinkOps {
        MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: None,
            }],
            links: vec![LinkInfo {
                index: 2,
                name: "eth0".into(),
            }],
            addrs: vec![AddrInfo {
                if_index: 2,
                addr: Ipv4Addr::new(192, 168, 1, 100),
            }],
            ..Default::default()
        })
    }

    fn mock_nft() -> MockNfTablesOps {
        MockNfTablesOps::new()
    }

    // ====================================================================
    // Firewall rule lifecycle tests
    // ====================================================================

    #[test]
    fn setup_nft_creates_rules() {
        let nft = mock_nft();

        nft.setup_fwmark_rules(1000, "eth0", FW_MARK, Ipv4Addr::new(192, 168, 1, 100))
            .unwrap();

        let state = nft.state.lock().unwrap();
        assert!(state.rules_active);
        let params = state.setup_params.as_ref().unwrap();
        assert_eq!(params.0, 1000);
        assert_eq!(params.1, "eth0");
        assert_eq!(params.2, FW_MARK);
        assert_eq!(params.3, Ipv4Addr::new(192, 168, 1, 100));
    }

    #[test]
    fn teardown_nft_removes_rules() {
        let nft = mock_nft();

        // Setup first
        nft.setup_fwmark_rules(1000, "eth0", FW_MARK, Ipv4Addr::new(192, 168, 1, 100))
            .unwrap();

        // Teardown
        nft.teardown_rules("eth0", FW_MARK, Ipv4Addr::new(192, 168, 1, 100))
            .unwrap();

        let state = nft.state.lock().unwrap();
        assert!(!state.rules_active);
        assert!(state.setup_params.is_none());
    }

    // ====================================================================
    // Fwmark infrastructure tests
    // ====================================================================

    #[tokio::test]
    async fn setup_fwmark_creates_route_and_rule() {
        let netlink = mock_netlink_with_wan();
        let nft = mock_nft();
        let worker = mock_worker();

        let infra = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft)
            .await
            .unwrap();

        // Verify TABLE_ID default route was added
        let nl_state = netlink.state.lock().unwrap();
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert_eq!(table_routes.len(), 1);
        assert_eq!(table_routes[0].destination, Ipv4Addr::UNSPECIFIED);
        assert_eq!(table_routes[0].gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));

        // Verify fwmark rule was added
        assert_eq!(nl_state.rules.len(), 1);
        assert_eq!(nl_state.rules[0].fw_mark, FW_MARK);
        assert_eq!(nl_state.rules[0].table_id, TABLE_ID);
        assert_eq!(nl_state.rules[0].priority, RULE_PRIORITY);

        // Verify WAN info
        assert_eq!(infra.wan_info.if_name, "eth0");
        assert_eq!(infra.wan_info.gateway, Ipv4Addr::new(192, 168, 1, 1));

        // Verify firewall rules were set up
        let nft_state = nft.state.lock().unwrap();
        assert!(nft_state.rules_active);

        // Mark as torn down to suppress Drop warning
        drop(infra);
    }

    #[tokio::test]
    async fn setup_fwmark_rolls_back_nft_on_route_failure() {
        let netlink = mock_netlink_with_wan();
        {
            let mut state = netlink.state.lock().unwrap();
            state
                .fail_on
                .insert("route_add".into(), "simulated route failure".into());
        }
        let nft = mock_nft();
        let worker = mock_worker();

        let result = setup_fwmark_infrastructure_with(&worker, netlink, &nft).await;
        assert!(result.is_err());

        // Firewall rules should be rolled back
        let nft_state = nft.state.lock().unwrap();
        assert!(!nft_state.rules_active);
    }

    #[tokio::test]
    async fn setup_fwmark_rolls_back_route_and_nft_on_rule_failure() {
        let netlink = mock_netlink_with_wan();
        {
            let mut state = netlink.state.lock().unwrap();
            state.fail_on.insert("rule_add".into(), "simulated rule failure".into());
        }
        let nft = mock_nft();
        let worker = mock_worker();

        let result = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft).await;
        assert!(result.is_err());

        // TABLE_ID route should be rolled back
        let nl_state = netlink.state.lock().unwrap();
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Firewall rules should be rolled back
        let nft_state = nft.state.lock().unwrap();
        assert!(!nft_state.rules_active);
    }

    #[tokio::test]
    async fn teardown_fwmark_removes_all_resources() {
        let netlink = mock_netlink_with_wan();
        let nft = mock_nft();
        let worker = mock_worker();

        let infra = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft)
            .await
            .unwrap();

        teardown_fwmark_infrastructure_with(infra, &nft).await;

        // Verify rule was removed
        let nl_state = netlink.state.lock().unwrap();
        assert!(nl_state.rules.is_empty());

        // Verify TABLE_ID route was removed
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Verify firewall rules were torn down
        let nft_state = nft.state.lock().unwrap();
        assert!(!nft_state.rules_active);
    }

    #[tokio::test]
    async fn cleanup_stale_removes_stale_entries() {
        let netlink = MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: Some(TABLE_ID),
            }],
            rules: vec![RuleSpec {
                fw_mark: FW_MARK,
                table_id: TABLE_ID,
                priority: RULE_PRIORITY,
            }],
            ..Default::default()
        });

        let nft = MockNfTablesOps::with_state(NfTablesState {
            rules_active: true,
            ..Default::default()
        });

        cleanup_stale_fwmark_rules_with(&netlink, &nft).await;

        // Verify stale rule was removed
        let nl_state = netlink.state.lock().unwrap();
        assert!(nl_state.rules.is_empty());

        // Verify stale route was removed
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Verify stale firewall rules were cleaned up
        let nft_state = nft.state.lock().unwrap();
        assert!(!nft_state.rules_active);
    }

    #[tokio::test]
    async fn cleanup_stale_is_idempotent_on_empty_state() {
        let netlink = MockNetlinkOps::new();
        let nft = MockNfTablesOps::new();

        // Should not panic or error
        cleanup_stale_fwmark_rules_with(&netlink, &nft).await;
    }

    // ====================================================================
    // get_wan_info tests
    // ====================================================================

    #[tokio::test]
    async fn get_wan_info_finds_default_route() {
        let netlink = mock_netlink_with_wan();
        let info = get_wan_info(&netlink).await.unwrap();

        assert_eq!(info.if_index, 2);
        assert_eq!(info.if_name, "eth0");
        assert_eq!(info.gateway, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(info.ip_addr, Ipv4Addr::new(192, 168, 1, 100));
    }

    #[tokio::test]
    async fn get_wan_info_fails_without_routes() {
        let netlink = MockNetlinkOps::new();
        let result = get_wan_info(&netlink).await;
        assert!(result.is_err());
    }

    // ====================================================================
    // get_vpn_info tests
    // ====================================================================

    #[tokio::test]
    async fn get_vpn_info_finds_wg_interface() {
        let netlink = MockNetlinkOps::with_state(NetlinkState {
            links: vec![
                LinkInfo {
                    index: 2,
                    name: "eth0".into(),
                },
                LinkInfo {
                    index: 5,
                    name: wireguard::WG_INTERFACE.into(),
                },
            ],
            ..Default::default()
        });

        let info = get_vpn_info(&netlink, "10.128.0.5/32", 9).await.unwrap();
        assert_eq!(info.if_index, 5);
        assert_eq!(info.cidr.first_address(), Ipv4Addr::new(10, 128, 0, 0));
        assert_eq!(info.cidr.network_length(), 9);
    }

    #[tokio::test]
    async fn get_vpn_info_fails_without_wg_interface() {
        let netlink = MockNetlinkOps::with_state(NetlinkState {
            links: vec![LinkInfo {
                index: 2,
                name: "eth0".into(),
            }],
            ..Default::default()
        });

        let result = get_vpn_info(&netlink, "10.128.0.5/32", 9).await;
        assert!(result.is_err());
    }

    // ====================================================================
    // Router lifecycle tests
    // ====================================================================

    fn make_router(
        netlink: MockNetlinkOps,
        route_ops: MockRouteOps,
        wg: MockWgOps,
    ) -> Router<MockNetlinkOps, MockRouteOps, MockWgOps> {
        Router {
            state_home: Arc::new(PathBuf::from("/tmp/test")),
            wg_data: test_wg_data(),
            netlink,
            route_ops,
            wg,
            wan_info: wan_info(),
            if_indices: None,
        }
    }

    fn test_wg_data() -> event::WireGuardData {
        use gnosis_vpn_lib::wireguard;
        event::WireGuardData {
            wg: wireguard::WireGuard::new(
                wireguard::Config {
                    listen_port: Some(51820),
                    allowed_ips: Some("0.0.0.0/0".into()),
                    force_private_key: None,
                },
                wireguard::KeyPair {
                    priv_key: "test_priv_key".into(),
                    public_key: "test_pub_key".into(),
                },
            ),
            interface_info: wireguard::InterfaceInfo {
                address: "10.128.0.5/32".into(),
            },
            peer_info: wireguard::PeerInfo {
                public_key: "test_peer_pub_key".into(),
                preshared_key: "test_psk".into(),
                endpoint: "1.2.3.4:51820".into(),
            },
        }
    }

    fn mock_netlink_with_wan_and_wg() -> MockNetlinkOps {
        MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: None,
            }],
            links: vec![
                LinkInfo {
                    index: 2,
                    name: "eth0".into(),
                },
                LinkInfo {
                    index: 5,
                    name: wireguard::WG_INTERFACE.into(),
                },
            ],
            addrs: vec![AddrInfo {
                if_index: 2,
                addr: Ipv4Addr::new(192, 168, 1, 100),
            }],
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn router_setup_creates_routes() {
        let netlink = mock_netlink_with_wan_and_wg();
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), route_ops.clone(), wg.clone());

        router.setup().await.unwrap();

        let nl_state = netlink.state.lock().unwrap();

        // VPN subnet route in TABLE_ID
        let table_vpn: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID) && r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert_eq!(table_vpn.len(), 1);

        // VPN subnet route in main table
        let main_vpn: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id.is_none() && r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert_eq!(main_vpn.len(), 1);

        // Default route replaced to VPN interface
        let defaults: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id.is_none() && r.prefix_len == 0)
            .collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].if_index, 5); // VPN interface

        // WG was brought up
        let wg_state = wg.state.lock().unwrap();
        assert!(wg_state.wg_up);

        // Cache was flushed (at least twice - before and after default route change)
        let route_state = route_ops.state.lock().unwrap();
        assert!(route_state.cache_flush_count >= 2);
    }

    #[tokio::test]
    async fn router_setup_rolls_back_on_vpn_route_failure() {
        let netlink = mock_netlink_with_wan_and_wg();
        {
            let mut state = netlink.state.lock().unwrap();
            state.fail_on.insert("route_add".into(), "simulated failure".into());
        }
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), route_ops, wg.clone());

        let result = router.setup().await;
        assert!(result.is_err());

        // WG should be brought down (rollback)
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up);
    }

    #[tokio::test]
    async fn router_setup_rejects_double_setup() {
        let netlink = mock_netlink_with_wan_and_wg();
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink, route_ops, wg);

        router.setup().await.unwrap();
        let result = router.setup().await;
        assert!(result.is_err());
        assert!(format!("{:?}", result.unwrap_err()).contains("already set up"));
    }

    #[tokio::test]
    async fn router_teardown_restores_routes() {
        let netlink = mock_netlink_with_wan_and_wg();
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), route_ops.clone(), wg.clone());

        router.setup().await.unwrap();

        // Reset flush count to isolate teardown flushes
        {
            let mut s = route_ops.state.lock().unwrap();
            s.cache_flush_count = 0;
        }

        router.teardown(Logs::Suppress).await.unwrap();

        let nl_state = netlink.state.lock().unwrap();

        // Default route should be back to WAN
        let defaults: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id.is_none() && r.prefix_len == 0)
            .collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].if_index, 2); // WAN interface
        assert_eq!(defaults[0].gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));

        // VPN subnet routes should be removed
        let vpn_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert!(vpn_routes.is_empty());

        // WG should be down
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up);

        // Cache should be flushed
        let route_state = route_ops.state.lock().unwrap();
        assert!(route_state.cache_flush_count >= 1);
    }

    // ====================================================================
    // FallbackRouter lifecycle tests
    // ====================================================================

    fn make_fallback_router(route_ops: MockRouteOps, wg: MockWgOps) -> FallbackRouter<MockRouteOps, MockWgOps> {
        FallbackRouter {
            state_home: Arc::new(PathBuf::from("/tmp/test")),
            wg_data: test_wg_data(),
            peer_ips: vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)],
            route_ops,
            wg,
            bypass_manager: None,
            vpn_subnet_route_added: false,
        }
    }

    #[tokio::test]
    async fn fallback_setup_adds_bypass_routes_then_wg_up() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        let state = route_ops.state.lock().unwrap();

        // 2 peer IP + 4 RFC1918 bypass + 1 VPN subnet = 7 total
        assert_eq!(state.added_routes.len(), 7);

        // First two are peer IPs
        assert_eq!(state.added_routes[0].0, "1.2.3.4");
        assert_eq!(state.added_routes[1].0, "5.6.7.8");

        // Then RFC1918
        assert_eq!(state.added_routes[2].0, "10.0.0.0/8");
        assert_eq!(state.added_routes[3].0, "172.16.0.0/12");

        // VPN subnet route (last)
        assert_eq!(state.added_routes[6].0, "10.128.0.0/9");
        assert_eq!(state.added_routes[6].2, wireguard::WG_INTERFACE);

        // WG should be up
        let wg_state = wg.state.lock().unwrap();
        assert!(wg_state.wg_up);
    }

    #[tokio::test]
    async fn fallback_wg_failure_rolls_back_bypass_routes() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::with_state(WgState {
            fail_on: {
                let mut m = std::collections::HashMap::new();
                m.insert("wg_quick_up".into(), "simulated wg failure".into());
                m
            },
            ..Default::default()
        });

        let mut router = make_fallback_router(route_ops.clone(), wg);
        let result = router.setup().await;
        assert!(result.is_err());

        // Bypass routes should be rolled back
        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty());
    }

    #[tokio::test]
    async fn fallback_teardown_wg_down_then_bypass_cleanup() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();
        router.teardown(Logs::Suppress).await.unwrap();

        let state = route_ops.state.lock().unwrap();
        // Bypass routes should be cleaned up
        assert!(state.added_routes.is_empty());

        // WG should be down
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up);
    }

    #[tokio::test]
    async fn fallback_teardown_cleans_bypass_even_if_wg_down_fails() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        // Make wg_quick_down fail
        {
            let mut s = wg.state.lock().unwrap();
            s.fail_on
                .insert("wg_quick_down".into(), "simulated wg down failure".into());
        }

        let result = router.teardown(Logs::Suppress).await;
        // Should return the wg error
        assert!(result.is_err());

        // But bypass routes should still be cleaned up
        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty());
    }

    #[tokio::test]
    async fn fallback_setup_wg_config_has_no_routing_postup() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops, wg.clone());
        router.setup().await.unwrap();

        let wg_state = wg.state.lock().unwrap();
        let config = wg_state.last_wg_config.as_ref().unwrap();
        // IPv6 blackhole PostUp for leak prevention is expected,
        // but routing-related PostUp hooks should not be present
        assert!(
            !config.contains("PostUp = ip route"),
            "wg config should not contain routing PostUp hooks, got:\n{config}"
        );
    }

    #[tokio::test]
    async fn fallback_setup_rolls_back_on_vpn_route_failure() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            fail_on_route_dest: {
                let mut m = std::collections::HashMap::new();
                m.insert("10.128.0.0/9".into(), "simulated VPN subnet route failure".into());
                m
            },
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        let result = router.setup().await;
        assert!(result.is_err(), "setup should fail when VPN subnet route fails");

        // WG should be brought back down (rollback)
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up, "WG should be down after rollback");

        // Bypass routes should be rolled back
        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty(), "bypass routes should be rolled back");
    }

    #[tokio::test]
    async fn fallback_teardown_removes_vpn_subnet_route() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        // Verify VPN subnet route exists before teardown
        {
            let state = route_ops.state.lock().unwrap();
            let vpn_count = state
                .added_routes
                .iter()
                .filter(|(dest, _, dev)| dest == "10.128.0.0/9" && dev == wireguard::WG_INTERFACE)
                .count();
            assert_eq!(vpn_count, 1, "VPN subnet route should exist before teardown");
        }

        router.teardown(Logs::Suppress).await.unwrap();

        // VPN subnet route should be removed
        let state = route_ops.state.lock().unwrap();
        let vpn_count = state
            .added_routes
            .iter()
            .filter(|(dest, _, dev)| dest == "10.128.0.0/9" && dev == wireguard::WG_INTERFACE)
            .count();
        assert_eq!(vpn_count, 0, "VPN subnet route should be removed after teardown");

        // Flag should be cleared
        assert!(!router.vpn_subnet_route_added);
    }

    // ====================================================================
    // Router (dynamic) rollback and resilience tests
    // ====================================================================

    #[tokio::test]
    async fn router_setup_rolls_back_on_default_route_failure() {
        let netlink = mock_netlink_with_wan_and_wg();
        {
            let mut state = netlink.state.lock().unwrap();
            // Allow route_add to succeed but fail route_replace (used for default route)
            state
                .fail_on
                .insert("route_replace".into(), "simulated route_replace failure".into());
        }
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), route_ops, wg.clone());

        let result = router.setup().await;
        assert!(result.is_err());

        // WG should be brought down (rollback)
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up, "WG should be down after rollback");

        // VPN TABLE_ID route should be cleaned up
        let nl_state = netlink.state.lock().unwrap();
        let table_vpn: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID) && r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert!(table_vpn.is_empty(), "VPN TABLE_ID route should be rolled back");
    }

    #[tokio::test]
    async fn router_teardown_continues_on_partial_failure() {
        let netlink = mock_netlink_with_wan_and_wg();
        let route_ops = MockRouteOps::new();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), route_ops.clone(), wg.clone());

        router.setup().await.unwrap();

        // Make route_replace fail (used to restore default route)
        {
            let mut state = netlink.state.lock().unwrap();
            state
                .fail_on
                .insert("route_replace".into(), "simulated restore-default failure".into());
        }

        // Teardown should still succeed overall (wg-quick down succeeds)
        // even though restoring the default route fails
        router.teardown(Logs::Suppress).await.unwrap();

        // WG should be down despite partial failure
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up, "WG should be down after teardown");

        // TABLE_ID VPN route should still be cleaned up
        let nl_state = netlink.state.lock().unwrap();
        let table_vpn: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID) && r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert!(table_vpn.is_empty(), "VPN TABLE_ID route should be cleaned up");

        // Cache should have been flushed
        let route_state = route_ops.state.lock().unwrap();
        assert!(route_state.cache_flush_count >= 1, "cache should be flushed");
    }
}
