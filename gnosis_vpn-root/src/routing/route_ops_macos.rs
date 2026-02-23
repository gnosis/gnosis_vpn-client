//! macOS route operations using BSD route commands.
//!
//! [`DarwinRouteOps`] implements [`RouteOps`] using macOS-native routing.
//! Currently wraps the `route` command; a future iteration could use
//! PF_ROUTE sockets directly for CLI-free operation.

use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::route_ops::RouteOps;
use super::Error;

/// Production [`RouteOps`] for macOS backed by the `route` command.
#[derive(Clone)]
pub struct DarwinRouteOps;

#[async_trait]
impl RouteOps for DarwinRouteOps {
    async fn get_default_interface(&self) -> Result<(String, Option<String>), Error> {
        let output = Command::new("route")
            .arg("-n")
            .arg("get")
            .arg("0.0.0.0")
            .run_stdout(Logs::Print)
            .await?;

        // Use shared parser with macOS-specific keys and suffix filter
        // (filters out "index:" when gateway shows "gateway: index: 28")
        super::parse_key_value_output(&output, "interface:", "gateway:", Some(":"))
    }

    async fn route_add(
        &self,
        dest: &str,
        gateway: Option<&str>,
        device: &str,
    ) -> Result<(), Error> {
        let mut cmd = Command::new("route");
        cmd.arg("-n").arg("add").arg("-inet").arg(dest);

        if let Some(gw) = gateway {
            cmd.arg(gw);
        }

        // macOS uses -interface for device specification
        cmd.arg("-interface").arg(device);
        cmd.run_stdout(Logs::Print).await?;
        Ok(())
    }

    async fn route_del(&self, dest: &str, _device: &str) -> Result<(), Error> {
        Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-inet")
            .arg(dest)
            .run_stdout(Logs::Suppress)
            .await?;
        Ok(())
    }

    async fn flush_routing_cache(&self) -> Result<(), Error> {
        // macOS does not have a routing cache to flush
        Ok(())
    }
}
