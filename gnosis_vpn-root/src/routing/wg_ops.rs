//! WireGuard interface management abstraction.
//!
//! Defines [`WgOps`] trait for wg-quick operations.
//! This is the only routing abstraction that still uses an external CLI tool
//! (wg-quick).
//!
//! Production code uses [`RealWgOps`].
//! Tests use stateful mocks (see `mocks` module).

use async_trait::async_trait;
use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext::Logs;

use super::Error;

use crate::wg_tooling;

/// Abstraction over WireGuard interface management.
#[async_trait]
pub trait WgOps: Send + Sync + Clone {
    /// Bring up WireGuard via wg-quick. Returns the resolved interface name.
    async fn wg_quick_up(&self, state_home: PathBuf, config: String) -> Result<String, Error>;

    /// Bring down WireGuard via wg-quick.
    async fn wg_quick_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error>;
}

/// Production [`WgOps`] that delegates to `wg_tooling`.
#[derive(Clone)]
pub struct RealWgOps;

#[async_trait]
impl WgOps for RealWgOps {
    async fn wg_quick_up(&self, state_home: PathBuf, config: String) -> Result<String, Error> {
        let iface = wg_tooling::up(state_home, config).await?;
        Ok(iface)
    }

    async fn wg_quick_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(state_home, logs).await?;
        Ok(())
    }
}
