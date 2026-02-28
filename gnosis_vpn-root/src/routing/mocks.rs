//! Stateful mocks for routing trait abstractions.
//!
//! These mocks track actual state (routes, rules, chains that exist) rather than
//! just verifying call sequences. This lets tests assert on the system's _state_
//! after a lifecycle operation, not just which calls happened.
//!
//! All mocks use `Arc<Mutex<_>>` for interior mutability in async contexts.

use async_trait::async_trait;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gnosis_vpn_lib::shell_command_ext::Logs;

use super::Error;
#[cfg(target_os = "linux")]
use super::netlink_ops::{AddrInfo, LinkInfo, NetlinkOps, RouteSpec, RuleSpec};
#[cfg(target_os = "linux")]
use super::nftables_ops::NfTablesOps;
use gnosis_vpn_lib::wireguard;

use super::route_ops::RouteOps;
use super::wg_ops::WgOps;

// ============================================================================
// MockNetlinkOps (Linux only)
// ============================================================================

#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct NetlinkState {
    pub routes: Vec<RouteSpec>,
    pub rules: Vec<RuleSpec>,
    pub links: Vec<LinkInfo>,
    pub addrs: Vec<AddrInfo>,
    /// Map of operation name -> error message. If set, the operation will fail.
    pub fail_on: HashMap<String, String>,
}

#[cfg(target_os = "linux")]
impl NetlinkState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::General(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct MockNetlinkOps {
    pub state: Arc<Mutex<NetlinkState>>,
}

#[cfg(target_os = "linux")]
impl MockNetlinkOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(NetlinkState::default())),
        }
    }

    pub fn with_state(state: NetlinkState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

#[cfg(target_os = "linux")]
#[async_trait]
impl NetlinkOps for MockNetlinkOps {
    async fn route_add(&self, route: &RouteSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_add")?;

        // Check for duplicate (same dest+prefix+table)
        let exists = s.routes.iter().any(|r| {
            r.destination == route.destination && r.prefix_len == route.prefix_len && r.table_id == route.table_id
        });
        if exists {
            return Err(Error::General(format!(
                "route already exists: {}/{}",
                route.destination, route.prefix_len
            )));
        }
        s.routes.push(route.clone());
        Ok(())
    }

    async fn route_replace(&self, route: &RouteSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_replace")?;

        // Remove existing route with same dest+prefix+table, then add
        s.routes.retain(|r| {
            !(r.destination == route.destination && r.prefix_len == route.prefix_len && r.table_id == route.table_id)
        });
        s.routes.push(route.clone());
        Ok(())
    }

    async fn route_del(&self, route: &RouteSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_del")?;

        let before = s.routes.len();
        s.routes.retain(|r| {
            !(r.destination == route.destination && r.prefix_len == route.prefix_len && r.table_id == route.table_id)
        });
        if s.routes.len() == before {
            return Err(Error::General("route not found".into()));
        }
        Ok(())
    }

    async fn route_list(&self, table_id: Option<u32>) -> Result<Vec<RouteSpec>, Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("route_list")?;

        Ok(match table_id {
            Some(id) => s.routes.iter().filter(|r| r.table_id == Some(id)).cloned().collect(),
            None => s.routes.clone(),
        })
    }

    async fn rule_add(&self, rule: &RuleSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("rule_add")?;
        s.rules.push(rule.clone());
        Ok(())
    }

    async fn rule_del(&self, rule: &RuleSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("rule_del")?;

        let before = s.rules.len();
        s.rules
            .retain(|r| !(r.fw_mark == rule.fw_mark && r.table_id == rule.table_id));
        if s.rules.len() == before {
            return Err(Error::General("rule not found".into()));
        }
        Ok(())
    }

    async fn rule_list_v4(&self) -> Result<Vec<RuleSpec>, Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("rule_list_v4")?;
        Ok(s.rules.clone())
    }

    async fn link_list(&self) -> Result<Vec<LinkInfo>, Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("link_list")?;
        Ok(s.links.clone())
    }

    async fn addr_list_v4(&self) -> Result<Vec<AddrInfo>, Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("addr_list_v4")?;
        Ok(s.addrs.clone())
    }
}

// ============================================================================
// MockNfTablesOps (Linux only)
// ============================================================================

#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone)]
pub struct NfTablesState {
    /// Whether fwmark rules are currently set up
    pub rules_active: bool,
    /// Parameters used for setup (for verification)
    pub setup_params: Option<(u32, String, u32, Ipv4Addr)>, // (vpn_uid, wan_if, fw_mark, snat_ip)
    pub fail_on: HashMap<String, String>,
}

#[cfg(target_os = "linux")]
impl NfTablesState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::NfTables(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub struct MockNfTablesOps {
    pub state: Arc<Mutex<NfTablesState>>,
}

#[cfg(target_os = "linux")]
impl MockNfTablesOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(NfTablesState::default())),
        }
    }

    pub fn with_state(state: NfTablesState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

#[cfg(target_os = "linux")]
impl NfTablesOps for MockNfTablesOps {
    fn setup_fwmark_rules(
        &self,
        vpn_uid: u32,
        wan_if_name: &str,
        fw_mark: u32,
        snat_ip: Ipv4Addr,
    ) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("setup_fwmark_rules")?;
        s.rules_active = true;
        s.setup_params = Some((vpn_uid, wan_if_name.to_string(), fw_mark, snat_ip));
        Ok(())
    }

    fn teardown_rules(&self, _wan_if_name: &str, _fw_mark: u32, _snat_ip: Ipv4Addr) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("teardown_rules")?;
        s.rules_active = false;
        s.setup_params = None;
        Ok(())
    }

    fn cleanup_stale_rules(&self, _fw_mark: u32) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("cleanup_stale_rules")?;
        s.rules_active = false;
        s.setup_params = None;
        Ok(())
    }
}

// ============================================================================
// MockRouteOps
// ============================================================================

#[derive(Debug, Default)]
pub struct RouteOpsState {
    pub added_routes: Vec<(String, Option<String>, String)>, // (dest, gateway, device)
    pub default_iface: Option<(String, Option<String>)>,     // (device, gateway)
    pub cache_flush_count: u32,
    pub fail_on: HashMap<String, String>,
    /// Destinations that should fail on route_add (for targeted failure injection).
    pub fail_on_route_dest: HashMap<String, String>,
}

impl RouteOpsState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::General(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct MockRouteOps {
    pub state: Arc<Mutex<RouteOpsState>>,
}

impl MockRouteOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RouteOpsState::default())),
        }
    }

    pub fn with_state(state: RouteOpsState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

#[async_trait]
impl RouteOps for MockRouteOps {
    async fn get_default_interface(&self) -> Result<(String, Option<String>), Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("get_default_interface")?;
        s.default_iface
            .clone()
            .ok_or_else(|| Error::General("no default interface configured in mock".into()))
    }

    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_add")?;
        if let Some(msg) = s.fail_on_route_dest.get(dest) {
            return Err(Error::General(msg.clone()));
        }
        s.added_routes
            .push((dest.into(), gateway.map(Into::into), device.into()));
        Ok(())
    }

    async fn route_del(&self, dest: &str, device: &str) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_del")?;
        // Silently succeed if route doesn't exist, matching macOS DarwinRouteOps
        // behavior (Logs::Suppress) and Linux usage patterns where route_del is
        // called defensively before route_add for idempotency.
        s.added_routes.retain(|r| !(r.0 == dest && r.2 == device));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn flush_routing_cache(&self) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("flush_routing_cache")?;
        s.cache_flush_count += 1;
        Ok(())
    }
}

// ============================================================================
// MockWgOps
// ============================================================================

#[derive(Debug, Default)]
pub struct WgState {
    pub wg_up: bool,
    /// Track wg-quick config passed to up
    pub last_wg_config: Option<String>,
    pub fail_on: HashMap<String, String>,
}

impl WgState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::General(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct MockWgOps {
    pub state: Arc<Mutex<WgState>>,
}

impl MockWgOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(WgState::default())),
        }
    }

    pub fn with_state(state: WgState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

#[async_trait]
impl WgOps for MockWgOps {
    async fn wg_quick_up(&self, _state_home: PathBuf, config: String) -> Result<String, Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("wg_quick_up")?;
        s.wg_up = true;
        s.last_wg_config = Some(config);
        Ok(wireguard::WG_INTERFACE.to_string())
    }

    async fn wg_quick_down(&self, _state_home: PathBuf, _logs: Logs) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("wg_quick_down")?;
        s.wg_up = false;
        Ok(())
    }
}
