//! Stateful mocks for routing trait abstractions.
//!
//! These mocks track actual state (routes, rules, chains that exist) rather than
//! just verifying call sequences. This lets tests assert on the system's _state_
//! after a lifecycle operation, not just which calls happened.
//!
//! All mocks use `Arc<Mutex<_>>` for interior mutability in async contexts.

#![cfg(test)]

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gnosis_vpn_lib::shell_command_ext::Logs;

use super::Error;
use super::iptables_ops::IptablesOps;
use super::netlink_ops::{AddrInfo, LinkInfo, NetlinkOps, RouteSpec, RuleSpec};
use super::shell_ops::ShellOps;

// ============================================================================
// MockNetlinkOps
// ============================================================================

#[derive(Debug, Default)]
pub struct NetlinkState {
    pub routes: Vec<RouteSpec>,
    pub rules: Vec<RuleSpec>,
    pub links: Vec<LinkInfo>,
    pub addrs: Vec<AddrInfo>,
    /// Map of operation name -> error message. If set, the operation will fail.
    pub fail_on: HashMap<String, String>,
}

impl NetlinkState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::General(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct MockNetlinkOps {
    pub state: Arc<Mutex<NetlinkState>>,
}

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

#[async_trait]
impl NetlinkOps for MockNetlinkOps {
    async fn route_add(&self, route: &RouteSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_add")?;

        // Check for duplicate (same dest+prefix+table)
        let exists = s.routes.iter().any(|r| {
            r.destination == route.destination
                && r.prefix_len == route.prefix_len
                && r.table_id == route.table_id
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
            !(r.destination == route.destination
                && r.prefix_len == route.prefix_len
                && r.table_id == route.table_id)
        });
        s.routes.push(route.clone());
        Ok(())
    }

    async fn route_del(&self, route: &RouteSpec) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("route_del")?;

        let before = s.routes.len();
        s.routes.retain(|r| {
            !(r.destination == route.destination
                && r.prefix_len == route.prefix_len
                && r.table_id == route.table_id)
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
            Some(id) => s
                .routes
                .iter()
                .filter(|r| r.table_id == Some(id))
                .cloned()
                .collect(),
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
// MockIptablesOps
// ============================================================================

/// State: table -> chain -> rules
#[derive(Debug, Default, Clone)]
pub struct IptablesState {
    pub tables: HashMap<String, HashMap<String, Vec<String>>>,
    pub fail_on: HashMap<String, String>,
}

impl IptablesState {
    fn check_fail(&self, op: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(msg.clone().into())
        } else {
            Ok(())
        }
    }

    fn get_chain_mut(
        &mut self,
        table: &str,
        chain: &str,
    ) -> Result<&mut Vec<String>, Box<dyn std::error::Error>> {
        self.tables
            .get_mut(table)
            .and_then(|t| t.get_mut(chain))
            .ok_or_else(|| format!("chain {chain} not found in table {table}").into())
    }
}

pub struct MockIptablesOps {
    pub state: Arc<Mutex<IptablesState>>,
}

impl MockIptablesOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(IptablesState::default())),
        }
    }

    pub fn with_state(state: IptablesState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Pre-populate a table with standard built-in chains.
    pub fn with_builtin_chains(self) -> Self {
        {
            let mut s = self.state.lock().unwrap();
            // mangle table with OUTPUT chain
            s.tables
                .entry("mangle".into())
                .or_default()
                .entry("OUTPUT".into())
                .or_default();
            // nat table with POSTROUTING chain
            s.tables
                .entry("nat".into())
                .or_default()
                .entry("POSTROUTING".into())
                .or_default();
        }
        self
    }
}

impl IptablesOps for MockIptablesOps {
    fn chain_exists(&self, table: &str, chain: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let s = self.state.lock().unwrap();
        s.check_fail("chain_exists")?;
        Ok(s.tables
            .get(table)
            .is_some_and(|t| t.contains_key(chain)))
    }

    fn new_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("new_chain")?;
        s.tables
            .entry(table.into())
            .or_default()
            .insert(chain.into(), Vec::new());
        Ok(())
    }

    fn flush_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("flush_chain")?;
        let rules = s.get_chain_mut(table, chain)?;
        rules.clear();
        Ok(())
    }

    fn delete_chain(&self, table: &str, chain: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("delete_chain")?;
        s.tables
            .get_mut(table)
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("table {table} not found").into()
            })?
            .remove(chain)
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("chain {chain} not found").into()
            })?;
        Ok(())
    }

    fn append(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("append")?;
        let rules = s.get_chain_mut(table, chain)?;
        rules.push(rule.into());
        Ok(())
    }

    fn delete(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("delete")?;
        let rules = s.get_chain_mut(table, chain)?;
        let before = rules.len();
        rules.retain(|r| r != rule);
        if rules.len() == before {
            return Err(format!("rule not found: {rule}").into());
        }
        Ok(())
    }

    fn exists(
        &self,
        table: &str,
        chain: &str,
        rule: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let s = self.state.lock().unwrap();
        s.check_fail("exists")?;
        Ok(s.tables
            .get(table)
            .and_then(|t| t.get(chain))
            .is_some_and(|rules| rules.iter().any(|r| r == rule)))
    }

    fn list(
        &self,
        table: &str,
        chain: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let s = self.state.lock().unwrap();
        s.check_fail("list")?;
        Ok(s.tables
            .get(table)
            .and_then(|t| t.get(chain))
            .cloned()
            .unwrap_or_default())
    }
}

// ============================================================================
// MockShellOps
// ============================================================================

#[derive(Debug, Default)]
pub struct ShellState {
    pub wg_up: bool,
    pub cache_flush_count: u32,
    pub added_routes: Vec<(String, Option<String>, String)>, // (dest, gateway, device)
    pub default_iface: Option<(String, Option<String>)>,     // (device, gateway)
    pub fail_on: HashMap<String, String>,
    /// Track wg-quick config passed to up
    pub last_wg_config: Option<String>,
}

impl ShellState {
    fn check_fail(&self, op: &str) -> Result<(), Error> {
        if let Some(msg) = self.fail_on.get(op) {
            Err(Error::General(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct MockShellOps {
    pub state: Arc<Mutex<ShellState>>,
}

impl MockShellOps {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellState::default())),
        }
    }

    pub fn with_state(state: ShellState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

#[async_trait]
impl ShellOps for MockShellOps {
    async fn flush_routing_cache(&self) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("flush_routing_cache")?;
        s.cache_flush_count += 1;
        Ok(())
    }

    async fn ip_route_show_default(&self) -> Result<(String, Option<String>), Error> {
        let s = self.state.lock().unwrap();
        s.check_fail("ip_route_show_default")?;
        s.default_iface
            .clone()
            .ok_or_else(|| Error::General("no default interface configured in mock".into()))
    }

    async fn wg_quick_up(&self, _state_home: PathBuf, config: String) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("wg_quick_up")?;
        s.wg_up = true;
        s.last_wg_config = Some(config);
        Ok(())
    }

    async fn wg_quick_down(&self, _state_home: PathBuf, _logs: Logs) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("wg_quick_down")?;
        s.wg_up = false;
        Ok(())
    }

    async fn ip_route_add(
        &self,
        dest: &str,
        gateway: Option<&str>,
        device: &str,
    ) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("ip_route_add")?;
        s.added_routes
            .push((dest.into(), gateway.map(Into::into), device.into()));
        Ok(())
    }

    async fn ip_route_del(&self, dest: &str, device: &str) -> Result<(), Error> {
        let mut s = self.state.lock().unwrap();
        s.check_fail("ip_route_del")?;
        let before = s.added_routes.len();
        s.added_routes.retain(|r| !(r.0 == dest && r.2 == device));
        if s.added_routes.len() == before {
            return Err(Error::General(format!(
                "route not found: {dest} dev {device}"
            )));
        }
        Ok(())
    }
}
