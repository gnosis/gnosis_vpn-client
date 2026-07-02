//! WireGuard interface management abstraction.
//!
//! Defines [`WgOps`] trait for the native WireGuard bring-up/teardown
//! implemented in `crate::wg_tooling`.
//!
//! Production code uses [`RealWgOps`].

use async_trait::async_trait;
use std::path::PathBuf;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::Logs;

use super::Error;

use crate::wg_tooling;

/// Abstraction over WireGuard interface management.
#[async_trait]
pub trait WgOps: Send + Sync {
    /// Bring up the WireGuard interface. Returns the resolved interface name.
    async fn wg_up(&self, state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error>;

    /// Bring down the WireGuard interface.
    async fn wg_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error>;
}

/// Production [`WgOps`] that delegates to `wg_tooling`.
#[cfg(target_os = "linux")]
pub struct RealWgOps {
    /// Netlink handle for link/address/route operations; shared with the router.
    handle: rtnetlink::Handle,
}

#[cfg(target_os = "linux")]
impl RealWgOps {
    pub fn new(handle: rtnetlink::Handle) -> Self {
        Self { handle }
    }
}

#[cfg(target_os = "linux")]
#[async_trait]
impl WgOps for RealWgOps {
    async fn wg_up(&self, state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error> {
        let iface = wg_tooling::up(&self.handle, state_home, wg_data).await?;
        Ok(iface)
    }

    async fn wg_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(&self.handle, state_home, logs).await?;
        Ok(())
    }
}

/// Production [`WgOps`] that delegates to `wg_tooling`.
#[cfg(target_os = "macos")]
pub struct RealWgOps;

#[cfg(target_os = "macos")]
#[async_trait]
impl WgOps for RealWgOps {
    async fn wg_up(&self, state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error> {
        let iface = wg_tooling::up(state_home, wg_data).await?;
        Ok(iface)
    }

    async fn wg_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(state_home, logs).await?;
        Ok(())
    }
}
