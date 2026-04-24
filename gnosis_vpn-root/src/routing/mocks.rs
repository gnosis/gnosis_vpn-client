//! Stateful mocks for routing trait abstractions.
//!
//! These mocks track actual state rather than just verifying call sequences.
//! This lets tests assert on the system's state after a lifecycle operation.
//!
//! All mocks use `Arc<Mutex<_>>` for interior mutability in async contexts.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gnosis_vpn_lib::shell_command_ext::Logs;

use super::Error;
use super::route_ops::RouteOps;
use super::wg_ops::WgOps;

// ============================================================================
// MockRouteOps
// ============================================================================

#[derive(Debug, Default)]
pub struct RouteOpsState {
    pub added_routes: Vec<(String, Option<String>, String)>, // (dest, gateway, device)
    pub default_iface: Option<(String, Option<String>)>,     // (device, gateway)
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
        // behavior and Linux usage patterns where route_del is called defensively
        // before route_add for idempotency.
        s.added_routes.retain(|r| !(r.0 == dest && r.2 == device));
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
        use gnosis_vpn_lib::wireguard;
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
