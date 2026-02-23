//! Abstraction over shell command operations for testability.
//!
//! Defines [`ShellOps`] trait covering shell commands used in routing:
//! - `ip route flush cache`
//! - `ip route show default`
//! - `wg-quick up/down`
//! - `ip route add/del` (for bypass routes)
//!
//! Production code uses [`RealShellOps`].
//! Tests use stateful mocks (see `mocks` module).

use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::Error;

use crate::wg_tooling;

/// Abstraction over shell commands used in routing.
///
/// Implementors must be cheaply cloneable (for sharing between
/// `FallbackRouter` and `BypassRouteManager`).
#[async_trait]
pub trait ShellOps: Send + Sync + Clone {
    /// Flush the kernel routing cache (`ip route flush cache`).
    async fn flush_routing_cache(&self) -> Result<(), Error>;

    /// Get the default WAN interface name and optional gateway.
    /// Equivalent to parsing `ip route show default`.
    async fn ip_route_show_default(&self) -> Result<(String, Option<String>), Error>;

    /// Bring up WireGuard via wg-quick.
    async fn wg_quick_up(&self, state_home: PathBuf, config: String) -> Result<(), Error>;

    /// Bring down WireGuard via wg-quick.
    async fn wg_quick_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error>;

    /// Add a route: `ip route add <dest> [via <gateway>] dev <device>`.
    async fn ip_route_add(
        &self,
        dest: &str,
        gateway: Option<&str>,
        device: &str,
    ) -> Result<(), Error>;

    /// Delete a route: `ip route del <dest> dev <device>`.
    async fn ip_route_del(&self, dest: &str, device: &str) -> Result<(), Error>;
}

/// Production [`ShellOps`] that executes real shell commands.
#[derive(Clone)]
pub struct RealShellOps;

#[async_trait]
impl ShellOps for RealShellOps {
    async fn flush_routing_cache(&self) -> Result<(), Error> {
        Command::new("ip")
            .arg("route")
            .arg("flush")
            .arg("cache")
            .run_stdout(Logs::Print)
            .await?;
        Ok(())
    }

    async fn ip_route_show_default(&self) -> Result<(String, Option<String>), Error> {
        let output = Command::new("ip")
            .arg("route")
            .arg("show")
            .arg("default")
            .run_stdout(Logs::Print)
            .await?;

        super::parse_key_value_output(&output, "dev", "via", None)
    }

    async fn wg_quick_up(&self, state_home: PathBuf, config: String) -> Result<(), Error> {
        wg_tooling::up(state_home, config).await?;
        Ok(())
    }

    async fn wg_quick_down(&self, state_home: PathBuf, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(state_home, logs).await?;
        Ok(())
    }

    async fn ip_route_add(
        &self,
        dest: &str,
        gateway: Option<&str>,
        device: &str,
    ) -> Result<(), Error> {
        let mut cmd = Command::new("ip");
        cmd.arg("route").arg("add").arg(dest);
        if let Some(gw) = gateway {
            cmd.arg("via").arg(gw);
        }
        cmd.arg("dev").arg(device);
        cmd.run_stdout(Logs::Print).await?;
        Ok(())
    }

    async fn ip_route_del(&self, dest: &str, device: &str) -> Result<(), Error> {
        Command::new("ip")
            .arg("route")
            .arg("del")
            .arg(dest)
            .arg("dev")
            .arg(device)
            .run_stdout(Logs::Suppress)
            .await?;
        Ok(())
    }
}
