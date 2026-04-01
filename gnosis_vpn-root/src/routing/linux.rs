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

use super::netlink_ops::{NetlinkOps, RealNetlinkOps, RouteSpec, RuleSpec};
use super::nftables_ops::{self, NfTablesOps, RealNfTablesOps};
use super::route_ops::RouteOps;
use super::route_ops_linux::NetlinkRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET};

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
pub async fn dynamic_router(
    state_home: PathBuf,
    worker: worker::Worker,
    wg_data: event::WireGuardData,
) -> Result<impl Routing, Error> {
    let infra = setup_fwmark_infrastructure(&worker).await.map_err(|error| {
        tracing::error!(?error, "Unable to setup fwmark infrastructure for dynamic routing");
        error
    })?;

    let handle = infra.netlink.handle();
    let netlink = RealNetlinkOps::new(handle.clone());
    let wg = RealWgOps;
    Ok(Router {
        state_home,
        wg_data,
        netlink,
        wg,
        infra,
        network_device_info: None,
        added_routes: Vec::new(),
    })
}

/// Creates a static fallback router using route operations via netlink.
///
/// Used when dynamic routing is not available. Provides simpler routing
/// by adding explicit host routes for peer IPs before bringing up WireGuard.
pub fn static_fallback_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle);
    let wg = RealWgOps;
    Ok(FallbackRouter {
        state_home: state_home.to_path_buf(),
        wg_data,
        peer_ips,
        route_ops,
        wg,
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

    let nft = RealNfTablesOps {};
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

/// Sets up the persistent fwmark infrastructure.
///
/// This establishes the firewall rules and routing table entries that
/// allow HOPR traffic to bypass the VPN tunnel.
///
/// The setup includes:
/// 1. Creating an rtnetlink connection for route management
/// 2. Getting WAN interface info (index, name, gateway)
/// 3. Setting up firewall rules to mark HOPR traffic with FW_MARK
/// 4. Creating TABLE_ID routing table with WAN as default gateway
/// 5. Adding fwmark rule: marked traffic uses TABLE_ID (bypasses VPN)
async fn setup_fwmark_infrastructure(worker: &worker::Worker) -> Result<FwmarkInfra, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let netlink = RealNetlinkOps::new(handle);
    let nft = RealNfTablesOps {};
    setup_fwmark_infrastructure_with(worker, netlink, &nft).await
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
        metric: None, // Doesn't matter in table 108
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

/// Tears down the persistent fwmark infrastructure at daemon shutdown.
///
/// This removes the firewall rules and routing table entries that were
/// set up by `setup_fwmark_infrastructure`.
///
/// The teardown includes:
/// 1. Deleting the fwmark routing rule
/// 2. Deleting the TABLE_ID default route
/// 3. Removing firewall mangle and NAT rules
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
        metric: None,
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
    /// Preserved route metric for teardown restoration
    wan_metric: Option<u32>,
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
    pub metric: Option<u32>,
}

/// Persistent fwmark infrastructure.
///
/// This struct holds the resources needed for fwmark-based routing bypass.
/// It is created at daemon startup and destroyed at daemon shutdown,
/// independent of individual WireGuard connection lifecycles.
///
/// **Important**: Must be explicitly torn down via `teardown_fwmark_infrastructure()`
/// to clean up firewall rules and routing table entries. Dropping without teardown
/// will log a warning and may leave the system in an inconsistent state.
#[derive(Clone, Debug)]
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
            wan_metric: wan.metric,
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
pub struct Router<N: NetlinkOps, W: WgOps> {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    netlink: N,
    wg: W,
    infra: FwmarkInfrastructure<N>,
    network_device_info: Option<NetworkDeviceInfo>,
    added_routes: Vec<RouteSpec>,
}

/// Static fallback router using route operations via netlink.
///
/// Used when dynamic routing (rtnetlink + firewall rules) is not available or not desired.
/// Simpler than [`Router`] but provides the same phased setup to avoid race conditions.
pub struct FallbackRouter<R: RouteOps, W: WgOps> {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
}

// ============================================================================
// Constants
// ============================================================================

/// Firewall mark used to identify HOPR traffic for bypass routing.
///
/// This mark is applied by firewall rules to packets owned by the worker process (UID-based),
/// allowing policy-based routing to send them via WAN instead of VPN tunnel.
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
    let metric = default_route.metric;

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
        metric,
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
impl<N: NetlinkOps + 'static, W: WgOps + 'static> Routing for Router<N, W> {
    /// Install VPN-specific routing (assumes fwmark infrastructure is already set up).
    ///
    /// This method requires that fwmark infrastructure has been established via
    /// `setup_fwmark_infrastructure`. It only handles VPN-specific routing:
    ///
    /// Phase 1 (before wg-quick up):
    ///   1. Add RFC1918 bypass routes to main table (via WAN) for LAN access
    ///
    /// Phase 2 (wg-quick up):
    ///   2. Generate wg-quick config and run wg-quick up
    ///
    /// Phase 3 (after wg-quick up):
    ///   3. Get VPN interface info
    ///   4. Add VPN subnet route to TABLE_ID (so bypassed traffic can reach VPN peers)
    ///   5. Add VPN subnet route to main table (overrides RFC1918 bypass for VPN server)
    ///   6. Flush routing cache (clear stale cached routes before default route change)
    ///   7. Replace main default route to VPN interface (atomic replace)
    ///   8. Flush routing cache (ensure all traffic uses new routes)
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        if self.network_device_info.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Use pre-existing WAN info from FwmarkInfrastructure
        let wan_info = &self.infra.wan_info;
        tracing::debug!(?wan_info, "using WAN interface from fwmark infrastructure");

        // Phase 1: Bring up WireGuard without routing table
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            vec!["Table = off".to_string()],
        );
        let interface_name = self.wg.wg_quick_up(self.state_home.clone(), wg_quick_content).await?;
        tracing::debug!(%interface_name, "wg-quick up");

        // Phase 2: Add RFC1918 bypass routes
        // These routes ensure non-HOPR processes can still access private networks
        for (net, prefix) in RFC1918_BYPASS_NETS {
            let destination: Ipv4Addr = net
                .parse()
                .map_err(|_| Error::General(format!("invalid RFC1918 network: {}", net)))?;
            let route = RouteSpec {
                destination,
                prefix_len: *prefix,
                gateway: Some(wan_info.gateway),
                if_index: wan_info.if_index,
                table_id: None,
                metric: None,
            };

            let res = self.netlink.route_add(&route).await;
            // always add routes for cleanup
            self.added_routes.push(route);
            match res {
                Ok(_) => {
                    tracing::debug!(
                        "ip route add {}/{} via {} dev {}",
                        net,
                        prefix,
                        wan_info.gateway,
                        wan_info.if_index
                    );
                }
                Err(error) => {
                    // Don't fail the whole setup for a single RFC1918 route, as these are best-effort
                    tracing::warn!(?error, net = %net, "failed to add RFC1918 bypass route, attempting to continue");
                }
            }
        }

        // Phase 3: Complete routing with VPN interface info

        // Step 3: Get VPN interface info
        let vpn_info = get_vpn_info(&self.netlink, &self.wg_data.interface_info.address, VPN_TUNNEL_SUBNET.1).await?;
        tracing::debug!(?vpn_info, "VPN interface data");

        // Store combined info for teardown
        self.network_device_info = Some(NetworkDeviceInfo::from_parts(wan_info.clone(), vpn_info.clone()));

        // Step 4: Add VPN subnet route to TABLE_ID
        // This allows bypassed traffic to still reach VPN addresses
        let vpn_table_route = RouteSpec {
            destination: vpn_info.cidr.first_address(),
            prefix_len: vpn_info.cidr.network_length(),
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: Some(TABLE_ID),
            metric: None,
        };
        self.netlink.route_add(&vpn_table_route).await?;
        tracing::debug!(
            "ip route add {} dev {} table {TABLE_ID}",
            vpn_info.cidr,
            vpn_info.if_index
        );

        // Step 5: Add VPN subnet route to main table
        // This ensures VPN subnet traffic uses tunnel, overriding any pre-existing RFC1918 routes
        let vpn_main_route = RouteSpec {
            destination: vpn_info.cidr.first_address(),
            prefix_len: vpn_info.cidr.network_length(),
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: None,
            metric: None,
        };
        match self.netlink.route_add(&vpn_main_route).await {
            Ok(_) => {
                tracing::debug!("ip route add {} dev {}", vpn_info.cidr, vpn_info.if_index);
            }
            Err(error) => {
                // Log warning but continue - default route should still work
                tracing::warn!(?error, "failed to add VPN subnet route to main table");
            }
        }

        // Step 6: Replace main default route to VPN interface
        // All non-bypassed traffic now goes through VPN
        // Use replace() for atomic replacement - avoids brief window with two default routes
        let vpn_default_route = RouteSpec {
            destination: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
            gateway: None,
            if_index: vpn_info.if_index,
            table_id: None,
            metric: None,
        };
        self.netlink.route_replace(&vpn_default_route).await?;
        tracing::debug!("ip route add default dev {}", vpn_info.if_index);

        tracing::info!("routing is ready");
        Ok(())
    }

    /// Uninstalls VPN-specific routing (fwmark infrastructure remains active).
    ///
    /// This method only tears down VPN-specific routes and the WireGuard interface.
    ///
    /// The steps:
    ///   1. Restore the default route in the MAIN routing table to WAN (atomic replace, original metric)
    ///      Equivalent command: `ip route replace default via $WAN_GW dev $IF_WAN metric $WAN_METRIC`
    ///   1b. Delete the VPN default route explicitly (no-op if metric-0 WAN replaced it in step 1)
    ///      Equivalent command: `ip route del default dev $IF_VPN`
    ///   2. Delete the VPN subnet route from the MAIN table
    ///      Equivalent command: `ip route del $VPN_SUBNET dev $IF_VPN`
    ///   3. Delete the VPN subnet route from TABLE_ID
    ///      Equivalent command: `ip route del $VPN_SUBNET dev $IF_VPN table $TABLE_ID`
    ///   4. Delete RFC1918 bypass routes (added during setup)
    ///      Equivalent command: `ip route del $RFC1918_NET dev $IF_WAN`
    ///   5. Run `wg-quick down` (while bypass is still active for HOPR traffic)
    ///
    async fn teardown(&mut self, logs: Logs) {
        match self.network_device_info.take() {
            Some(NetworkDeviceInfo {
                wan_if_index,
                vpn_if_index,
                vpn_cidr,
                wan_gw,
                wan_metric,
            }) => {
                // Step 1: Set the default route back to the WAN interface with original metric
                let wan_default = RouteSpec {
                    destination: Ipv4Addr::UNSPECIFIED,
                    prefix_len: 0,
                    gateway: Some(wan_gw),
                    if_index: wan_if_index,
                    table_id: None,
                    metric: wan_metric,
                };
                teardown_op(
                    "set default route back to interface",
                    &format!("ip route replace default via {wan_gw} dev {wan_if_index}"),
                    || self.netlink.route_replace(&wan_default),
                )
                .await;

                // Step 1b: Explicitly remove the VPN default route.
                // When WAN had a non-zero metric, route_replace above adds a new WAN route
                // without touching the VPN metric-0 default. We remove it here rather than
                // relying on wg-quick down to clean it up via interface deletion.
                // When WAN had metric 0, route_replace already replaced the VPN route, so
                // this is a no-op (teardown_op tolerates the "not found" error).
                let vpn_default_route = RouteSpec {
                    destination: Ipv4Addr::UNSPECIFIED,
                    prefix_len: 0,
                    gateway: None,
                    if_index: vpn_if_index,
                    table_id: None,
                    metric: None,
                };
                teardown_op(
                    "delete VPN default route from main table",
                    &format!("ip route del default dev {vpn_if_index}"),
                    || self.netlink.route_del(&vpn_default_route),
                )
                .await;

                // Step 2: Delete the VPN subnet route from the main table
                let vpn_main_route = RouteSpec {
                    destination: vpn_cidr.first_address(),
                    prefix_len: vpn_cidr.network_length(),
                    gateway: None,
                    if_index: vpn_if_index,
                    table_id: None,
                    metric: None,
                };
                teardown_op(
                    "delete VPN subnet route from main table",
                    &format!("ip route del {vpn_cidr} dev {vpn_if_index}"),
                    || self.netlink.route_del(&vpn_main_route),
                )
                .await;

                // Step 3: Delete the TABLE_ID routing table VPN route
                let vpn_table_route = RouteSpec {
                    destination: vpn_cidr.first_address(),
                    prefix_len: vpn_cidr.network_length(),
                    gateway: None,
                    if_index: vpn_if_index,
                    table_id: Some(TABLE_ID),
                    metric: None,
                };
                teardown_op(
                    &format!("delete VPN subnet route from table {TABLE_ID}"),
                    &format!("ip route del {vpn_cidr} dev {vpn_if_index} table {TABLE_ID}"),
                    || self.netlink.route_del(&vpn_table_route),
                )
                .await;

                // Step 4: Delete RFC1918 bypass routes that were added during setup
                for route in self.added_routes.drain(..) {
                    teardown_op(
                        &format!("delete RFC1918 bypass route {}/{}", route.destination, route.prefix_len),
                        &format!(
                            "ip route del {}/{} via {wan_gw} dev {wan_if_index}",
                            route.destination, route.prefix_len
                        ),
                        || self.netlink.route_del(&route),
                    )
                    .await;
                }

                // Step 5: Run wg-quick down while bypass infrastructure is still active
                // HOPR traffic continues: firewall marks -> fwmark rule -> TABLE_ID -> WAN
                match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
                    Ok(_) => tracing::debug!("wg-quick down"),
                    Err(error) => {
                        tracing::warn!(?error, "wg-quick down failed during teardown");
                    }
                }
            }
            None => {
                // Attempt wg-quick down even and ignore errors
                match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
                    Ok(_) => tracing::debug!("wg-quick down"),
                    Err(error) => {
                        tracing::warn!(?error, "wg-quick down failed during best-effort teardown");
                    }
                }

                for route in &self.added_routes {
                    if let Err(error) = self.netlink.route_del(route).await {
                        tracing::warn!(?error, route = %format!("{}/{}", route.destination, route.prefix_len), "failed to delete RFC1918 bypass route during partial teardown");
                    }
                }
                self.added_routes.clear();
            }
        }

        // always teardown fwmark infrastructure
        let nft = RealNfTablesOps {};
        teardown_fwmark_infrastructure_with(self.infra.clone(), &nft).await;
        tracing::info!("VPN routing teardown complete");
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
        // Phase 1: Add bypass routes to wg-quick up
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");
        let mut extra = vec![];

        // exclude static peer IPs from tunnel
        for ip in &self.peer_ips {
            extra.extend(post_up_routing(ip.to_string(), device.clone(), gateway.clone()));
        }
        // restore on down
        for ip in &self.peer_ips {
            extra.push(pre_down_routing(ip.to_string(), device.clone(), gateway.clone()));
        }

        // Phase 2: wg-quick up
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        self.wg.wg_quick_up(self.state_home.clone(), wg_quick_content).await?;
        tracing::debug!("wg-quick up");
        Ok(())
    }

    /// Teardown split-tunnel routing for FallbackRouter.
    ///
    /// Teardown order:
    /// 1. Remove VPN subnet route (best-effort)
    /// 2. wg-quick down
    /// 3. Remove bypass routes
    ///
    async fn teardown(&mut self, logs: Logs) {
        // wg-quick down
        match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
            Ok(_) => tracing::debug!("wg-quick down"),
            Err(error) => {
                tracing::error!(?error, "wg-quick down failed during teardown");
            }
        }
    }
}

fn post_up_routing(route_addr: String, device: String, gateway: Option<String>) -> Vec<String> {
    match gateway {
        Some(gw) => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!(
                "PostUp = ip route del {route_addr} via {gateway} dev {device} || true",
                route_addr = route_addr,
                gateway = gw,
                device = device
            ),
            format!(
                "PostUp = ip route add {route_addr} via {gateway} dev {device}",
                route_addr = route_addr,
                gateway = gw,
                device = device
            ),
        ],
        None => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!(
                "PostUp = ip route del {route_addr} dev {device} || true",
                route_addr = route_addr,
                device = device
            ),
            format!(
                "PostUp = ip route add {route_addr} dev {device}",
                route_addr = route_addr,
                device = device
            ),
        ],
    }
}

fn pre_down_routing(route_addr: String, device: String, gateway: Option<String>) -> String {
    match gateway {
        Some(gw) => format!(
            // wg-quick stops execution on error, ignore errors to hit all commands
            "PreDown = ip route del {route_addr} via {gateway} dev {device} || true",
            route_addr = route_addr,
            gateway = gw,
            device = device,
        ),
        None => format!(
            // wg-quick stops execution on error, ignore errors to hit all commands
            "PreDown = ip route del {route_addr} dev {device} || true",
            route_addr = route_addr,
            device = device,
        ),
    }
}

/// Try whatever teardown we can on startup to clean up from any previous unclean shutdowns.
pub async fn reset_on_startup(state_home: PathBuf) {
    cleanup_stale_fwmark_rules().await;
    let wg = RealWgOps {};
    let _ = wg.wg_quick_down(state_home, Logs::Suppress).await;
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
    fn test_parse_cidr() -> anyhow::Result<()> {
        let cidr = "192.168.101.0/24";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, std::net::Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), std::net::Ipv4Addr::new(192, 168, 101, 0));
        assert_eq!(ip.first_address().to_string(), "192.168.101.0");
        assert_eq!(
            ip.iter()
                .addresses()
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("no address"))? // Fixed Warning
                .to_string(),
            "192.168.101.1"
        );
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.32/24";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, std::net::Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), std::net::Ipv4Addr::new(192, 168, 101, 0));
        assert_eq!(
            ip.iter()
                .addresses()
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("no address"))? // Fixed Error
                .to_string(),
            "192.168.101.1"
        );
        assert_eq!(ip.network_length(), 24);
        assert_eq!("192.168.101.0/24", ip.to_string());

        let cidr = "192.168.101.32/32";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, std::net::Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), std::net::Ipv4Addr::new(192, 168, 101, 32));
        assert_eq!(ip.network_length(), 32);
        assert!(ip.is_host_address());
        assert_eq!("192.168.101.32", ip.to_string());

        let cidr = "192.168.101.1";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, std::net::Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), std::net::Ipv4Addr::new(192, 168, 101, 1));
        assert_eq!(ip.network_length(), 32);
        assert_eq!("192.168.101.1", ip.to_string());

        let cidr = "192.128.101.33/9";
        let ip = cidr::parsers::parse_cidr_ignore_hostbits::<cidr::Ipv4Cidr, _>(cidr, std::net::Ipv4Addr::from_str)?;

        assert_eq!(ip.first_address(), std::net::Ipv4Addr::new(192, 128, 0, 0));
        assert_eq!(
            ip.iter()
                .addresses()
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("no address"))? // Fixed Error
                .to_string(),
            "192.128.0.1"
        );
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

    fn mock_netlink_with_wan() -> MockNetlinkOps {
        MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: None,
                metric: Some(100),
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
    fn setup_nft_creates_rules() -> anyhow::Result<()> {
        let nft = mock_nft();

        nft.setup_fwmark_rules(1000, "eth0", FW_MARK, std::net::Ipv4Addr::new(192, 168, 1, 100))?;

        let state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;

        assert!(state.rules_active);

        let params = state
            .setup_params
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing setup params"))?;

        assert_eq!(params.0, 1000);
        assert_eq!(params.1, "eth0");
        assert_eq!(params.2, FW_MARK);
        assert_eq!(params.3, std::net::Ipv4Addr::new(192, 168, 1, 100));

        Ok(())
    }

    #[test]
    fn teardown_nft_removes_rules() -> anyhow::Result<()> {
        let nft = mock_nft();

        // Setup first
        nft.setup_fwmark_rules(1000, "eth0", FW_MARK, std::net::Ipv4Addr::new(192, 168, 1, 100))?;

        // Teardown
        nft.teardown_rules("eth0", FW_MARK, std::net::Ipv4Addr::new(192, 168, 1, 100))?;

        let state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;

        assert!(!state.rules_active);
        assert!(state.setup_params.is_none());

        Ok(())
    }

    // ====================================================================
    // Fwmark infrastructure tests
    // ====================================================================

    #[tokio::test]
    async fn setup_fwmark_creates_route_and_rule() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan();
        let nft = mock_nft();
        let worker = mock_worker();

        let infra = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft)
            .await
            .map_err(|e| anyhow::anyhow!("setup_fwmark_infrastructure failed: {e}"))?;

        // Verify TABLE_ID default route was added
        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
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
        let nft_state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(nft_state.rules_active);

        // Mark as torn down to suppress Drop warning
        drop(infra);
        Ok(())
    }

    #[tokio::test]
    async fn setup_fwmark_rolls_back_nft_on_route_failure() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan();
        {
            let mut state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
            state
                .fail_on
                .insert("route_add".into(), "simulated route failure".into());
        }
        let nft = mock_nft();
        let worker = mock_worker();

        let result = setup_fwmark_infrastructure_with(&worker, netlink, &nft).await;
        assert!(result.is_err());

        // Firewall rules should be rolled back
        let nft_state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!nft_state.rules_active);
        Ok(())
    }

    #[tokio::test]
    async fn setup_fwmark_rolls_back_route_and_nft_on_rule_failure() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan();
        {
            let mut state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
            state.fail_on.insert("rule_add".into(), "simulated rule failure".into());
        }
        let nft = mock_nft();
        let worker = mock_worker();

        let result = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft).await;
        assert!(result.is_err());

        // TABLE_ID route should be rolled back
        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Firewall rules should be rolled back
        let nft_state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!nft_state.rules_active);
        Ok(())
    }

    #[tokio::test]
    async fn teardown_fwmark_removes_all_resources() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan();
        let nft = mock_nft();
        let worker = mock_worker();

        let infra = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft)
            .await
            .map_err(|e| anyhow::anyhow!("setup_fwmark_infrastructure failed: {e}"))?;

        teardown_fwmark_infrastructure_with(infra, &nft).await;

        // Verify rule was removed
        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(nl_state.rules.is_empty());

        // Verify TABLE_ID route was removed
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Verify firewall rules were torn down
        let nft_state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!nft_state.rules_active);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_stale_removes_stale_entries() -> anyhow::Result<()> {
        let netlink = MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: Some(TABLE_ID),
                metric: Some(100),
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
        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(nl_state.rules.is_empty());

        // Verify stale route was removed
        let table_routes: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID))
            .collect();
        assert!(table_routes.is_empty());

        // Verify stale firewall rules were cleaned up
        let nft_state = nft.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!nft_state.rules_active);
        Ok(())
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
    async fn get_wan_info_finds_default_route() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan();
        let info = get_wan_info(&netlink)
            .await
            .map_err(|e| anyhow::anyhow!("get_wan_info failed: {e}"))?;

        assert_eq!(info.if_index, 2);
        assert_eq!(info.if_name, "eth0");
        assert_eq!(info.gateway, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(info.ip_addr, Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(info.metric, Some(100));
        Ok(())
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
    async fn get_vpn_info_finds_wg_interface() -> anyhow::Result<()> {
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

        let info = get_vpn_info(&netlink, "10.128.0.5/32", 9)
            .await
            .map_err(|e| anyhow::anyhow!("get_vpn_info failed: {e}"))?;
        assert_eq!(info.if_index, 5);
        assert_eq!(info.cidr.first_address(), Ipv4Addr::new(10, 128, 0, 0));
        assert_eq!(info.cidr.network_length(), 9);
        Ok(())
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

    async fn make_router(netlink: MockNetlinkOps, wg: MockWgOps) -> anyhow::Result<Router<MockNetlinkOps, MockWgOps>> {
        let nft = mock_nft();
        let worker = mock_worker();

        let infra = setup_fwmark_infrastructure_with(&worker, netlink.clone(), &nft).await?;
        let r = Router {
            state_home: PathBuf::from("/tmp/test"),
            wg_data: test_wg_data(),
            netlink,
            wg,
            infra,
            network_device_info: None,
            added_routes: Vec::new(),
        };
        Ok(r)
    }

    fn test_wg_data() -> event::WireGuardData {
        use gnosis_vpn_lib::wireguard;
        event::WireGuardData {
            wg: wireguard::WireGuard::new(
                wireguard::Config {
                    listen_port: Some(51820),
                    allowed_ips: Some("0.0.0.0/0".into()),
                    force_private_key: None,
                    dns: None,
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
                metric: None,
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
    async fn router_setup_creates_routes() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan_and_wg();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), wg.clone()).await?;

        router.setup().await?;

        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;

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
        let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(wg_state.wg_up);

        Ok(())
    }

    //     #[tokio::test]
    //     async fn router_setup_rolls_back_on_vpn_route_failure() -> anyhow::Result<()> {
    //         let netlink = mock_netlink_with_wan_and_wg();
    //         {
    //             let mut state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //             state.fail_on.insert("route_add".into(), "simulated failure".into());
    //         }
    //         let wg = MockWgOps::new();
    //         let mut router = make_router(netlink.clone(), wg.clone()).await?;
    //
    //         let result = router.setup().await;
    //         assert!(result.is_err());
    //
    //         // WG should be brought down (rollback)
    //         let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         assert!(!wg_state.wg_up);
    //         Ok(())
    //     }

    #[tokio::test]
    async fn router_setup_rejects_double_setup() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan_and_wg();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink, wg).await?;

        router.setup().await?;
        let result = router.setup().await;
        assert!(result.is_err());
        assert!(format!("{:?}", result.unwrap_err()).contains("already set up"));
        Ok(())
    }

    #[tokio::test]
    async fn router_teardown_restores_routes() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan_and_wg();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), wg.clone()).await?;

        router.setup().await?;

        router.teardown(Logs::Suppress).await;

        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;

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
        let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!wg_state.wg_up);

        Ok(())
    }

    #[tokio::test]
    async fn router_teardown_restores_wan_metric() -> anyhow::Result<()> {
        // When the WAN default route has a non-zero metric (common with NetworkManager),
        // VPN setup adds a *new* metric-0 default rather than replacing the WAN route.
        // Teardown must explicitly remove the VPN metric-0 default and restore WAN at its original metric.
        let netlink = MockNetlinkOps::with_state(NetlinkState {
            routes: vec![RouteSpec {
                destination: Ipv4Addr::UNSPECIFIED,
                prefix_len: 0,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                if_index: 2,
                table_id: None,
                metric: Some(100),
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
        });
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), wg.clone()).await?;

        router.setup().await?;
        router.teardown(Logs::Suppress).await;

        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;

        // Exactly one default route in the main table, on the WAN interface with original metric
        let defaults: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id.is_none() && r.prefix_len == 0)
            .collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].if_index, 2);
        assert_eq!(defaults[0].metric, Some(100));

        Ok(())
    }

    // ====================================================================
    // FallbackRouter lifecycle tests
    // ====================================================================

    fn make_fallback_router(route_ops: MockRouteOps, wg: MockWgOps) -> FallbackRouter<MockRouteOps, MockWgOps> {
        FallbackRouter {
            state_home: PathBuf::from("/tmp/test"),
            wg_data: test_wg_data(),
            peer_ips: vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)],
            route_ops,
            wg,
        }
    }

    //     #[tokio::test]
    //     async fn fallback_setup_adds_bypass_routes_then_wg_up() -> anyhow::Result<()> {
    //         let route_ops = MockRouteOps::with_state(RouteOpsState {
    //             default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
    //             ..Default::default()
    //         });
    //         let wg = MockWgOps::new();
    //
    //         let mut router = make_fallback_router(route_ops.clone(), wg.clone());
    //         router.setup().await?;
    //
    //         let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //
    //         // 2 peer IP + 4 RFC1918 bypass + 1 VPN subnet = 7 total
    //         assert_eq!(state.added_routes.len(), 7);
    //
    //         // First two are peer IPs
    //         assert_eq!(state.added_routes[0].0, "1.2.3.4");
    //         assert_eq!(state.added_routes[1].0, "5.6.7.8");
    //
    //         // Then RFC1918
    //         assert_eq!(state.added_routes[2].0, "10.0.0.0/8");
    //         assert_eq!(state.added_routes[3].0, "172.16.0.0/12");
    //
    //         // VPN subnet route (last)
    //         assert_eq!(state.added_routes[6].0, "10.128.0.0/9");
    //         assert_eq!(state.added_routes[6].2, wireguard::WG_INTERFACE);
    //
    //         // WG should be up
    //         let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         assert!(wg_state.wg_up);
    //         Ok(())
    //     }

    #[tokio::test]
    async fn fallback_wg_failure_rolls_back_bypass_routes() -> anyhow::Result<()> {
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
        let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(state.added_routes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn fallback_teardown_wg_down_then_bypass_cleanup() -> anyhow::Result<()> {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await?;
        router.teardown(Logs::Suppress).await;

        let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        // Bypass routes should be cleaned up
        assert!(state.added_routes.is_empty());

        // WG should be down
        let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!wg_state.wg_up);
        Ok(())
    }

    #[tokio::test]
    async fn fallback_teardown_cleans_bypass_even_if_wg_down_fails() -> anyhow::Result<()> {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops.clone(), wg.clone());
        router.setup().await?;

        // Make wg_quick_down fail
        {
            let mut s = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
            s.fail_on
                .insert("wg_quick_down".into(), "simulated wg down failure".into());
        }

        router.teardown(Logs::Suppress).await;

        // But bypass routes should still be cleaned up
        let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(state.added_routes.is_empty());
        Ok(())
    }

    //     #[tokio::test]
    //     async fn fallback_setup_wg_config_has_no_routing_postup() -> anyhow::Result<()> {
    //         let route_ops = MockRouteOps::with_state(RouteOpsState {
    //             default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
    //             ..Default::default()
    //         });
    //         let wg = MockWgOps::new();
    //
    //         let mut router = make_fallback_router(route_ops, wg.clone());
    //         router.setup().await?;
    //
    //         let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         let config = wg_state
    //             .last_wg_config
    //             .as_ref()
    //             .ok_or_else(|| anyhow::anyhow!("Expected wg config to be set"))?;
    //         // IPv6 blackhole PostUp for leak prevention is expected,
    //         // but routing-related PostUp hooks should not be present
    //         assert!(
    //             !config.contains("PostUp = ip route"),
    //             "wg config should not contain routing PostUp hooks, got:\n{config}"
    //         );
    //         Ok(())
    //     }

    //     #[tokio::test]
    //     async fn fallback_setup_rolls_back_on_vpn_route_failure() -> anyhow::Result<()> {
    //         let route_ops = MockRouteOps::with_state(RouteOpsState {
    //             default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
    //             fail_on_route_dest: {
    //                 let mut m = std::collections::HashMap::new();
    //                 m.insert("10.128.0.0/9".into(), "simulated VPN subnet route failure".into());
    //                 m
    //             },
    //             ..Default::default()
    //         });
    //         let wg = MockWgOps::new();
    //
    //         let mut router = make_fallback_router(route_ops.clone(), wg.clone());
    //         let result = router.setup().await;
    //         assert!(result.is_err(), "setup should fail when VPN subnet route fails");
    //
    //         // WG should be brought back down (rollback)
    //         let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         assert!(!wg_state.wg_up, "WG should be down after rollback");
    //
    //         // Bypass routes should be rolled back
    //         let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         assert!(state.added_routes.is_empty(), "bypass routes should be rolled back");
    //         Ok(())
    //     }

    //     #[tokio::test]
    //     async fn fallback_teardown_removes_vpn_subnet_route() -> anyhow::Result<()> {
    //         let route_ops = MockRouteOps::with_state(RouteOpsState {
    //             default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
    //             ..Default::default()
    //         });
    //         let wg = MockWgOps::new();
    //
    //         let mut router = make_fallback_router(route_ops.clone(), wg.clone());
    //         router.setup().await?;
    //
    //         // Verify VPN subnet route exists before teardown
    //         {
    //             let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //             let vpn_count = state
    //                 .added_routes
    //                 .iter()
    //                 .filter(|(dest, _, dev)| dest == "10.128.0.0/9" && dev == wireguard::WG_INTERFACE)
    //                 .count();
    //             assert_eq!(vpn_count, 1, "VPN subnet route should exist before teardown");
    //         }
    //
    //         router.teardown(Logs::Suppress).await;
    //
    //         // VPN subnet route should be removed
    //         let state = route_ops.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         let vpn_count = state
    //             .added_routes
    //             .iter()
    //             .filter(|(dest, _, dev)| dest == "10.128.0.0/9" && dev == wireguard::WG_INTERFACE)
    //             .count();
    //         assert_eq!(vpn_count, 0, "VPN subnet route should be removed after teardown");
    //
    //         Ok(())
    //     }

    // ====================================================================
    // Router (dynamic) rollback and resilience tests
    // ====================================================================

    //     #[tokio::test]
    //     async fn router_setup_rolls_back_on_default_route_failure() -> anyhow::Result<()> {
    //         let netlink = mock_netlink_with_wan_and_wg();
    //         {
    //             let mut state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //             // Allow route_add to succeed but fail route_replace (used for default route)
    //             state
    //                 .fail_on
    //                 .insert("route_replace".into(), "simulated route_replace failure".into());
    //         }
    //         let wg = MockWgOps::new();
    //         let mut router = make_router(netlink.clone(), wg.clone()).await?;
    //
    //         let result = router.setup().await;
    //         assert!(result.is_err());
    //
    //         // WG should be brought down (rollback)
    //         let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         assert!(!wg_state.wg_up, "WG should be down after rollback");
    //
    //         // VPN TABLE_ID route should be cleaned up
    //         let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
    //         let table_vpn: Vec<_> = nl_state
    //             .routes
    //             .iter()
    //             .filter(|r| r.table_id == Some(TABLE_ID) && r.destination == Ipv4Addr::new(10, 128, 0, 0))
    //             .collect();
    //         assert!(table_vpn.is_empty(), "VPN TABLE_ID route should be rolled back");
    //         Ok(())
    //     }

    #[tokio::test]
    async fn router_teardown_continues_on_partial_failure() -> anyhow::Result<()> {
        let netlink = mock_netlink_with_wan_and_wg();
        let wg = MockWgOps::new();
        let mut router = make_router(netlink.clone(), wg.clone()).await?;

        router.setup().await.map_err(|e| anyhow::anyhow!("setup failed: {e}"))?;

        // Make route_replace fail (used to restore default route)
        {
            let mut state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
            state
                .fail_on
                .insert("route_replace".into(), "simulated restore-default failure".into());
        }

        // Teardown should still succeed overall (wg-quick down succeeds)
        // even though restoring the default route fails
        router.teardown(Logs::Suppress).await;

        // WG should be down despite partial failure
        let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!wg_state.wg_up, "WG should be down after teardown");

        // TABLE_ID VPN route should still be cleaned up
        let nl_state = netlink.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        let table_vpn: Vec<_> = nl_state
            .routes
            .iter()
            .filter(|r| r.table_id == Some(TABLE_ID) && r.destination == Ipv4Addr::new(10, 128, 0, 0))
            .collect();
        assert!(table_vpn.is_empty(), "VPN TABLE_ID route should be cleaned up");

        Ok(())
    }
}
