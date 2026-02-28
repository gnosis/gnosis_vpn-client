//! macOS route operations using BSD route commands.
//!
//! [`DarwinRouteOps`] implements [`RouteOps`] using macOS-native routing.
//! Currently wraps the `route` command; a future iteration could use
//! PF_ROUTE sockets directly for CLI-free operation.

use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::Error;
use super::route_ops::RouteOps;

/// Build the argument list for a `route add` invocation.
///
/// When a gateway is present, `-ifp` pins the route to the named interface.
/// Without a gateway, `-interface` marks the destination as directly reachable
/// via the named interface.
fn route_add_args(dest: &str, gateway: Option<&str>, device: &str) -> Vec<String> {
    let mut args = vec!["-n".into(), "add".into(), "-inet".into(), dest.into()];
    if let Some(gw) = gateway {
        args.push(gw.into());
        args.push("-ifp".into());
        args.push(device.into());
    } else {
        args.push("-interface".into());
        args.push(device.into());
    }
    args
}

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

    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error> {
        let mut cmd = Command::new("route");
        for arg in route_add_args(dest, gateway, device) {
            cmd.arg(arg);
        }
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

    #[cfg(target_os = "linux")]
    async fn flush_routing_cache(&self) -> Result<(), Error> {
        // macOS does not have a routing cache to flush
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_add_args_with_gateway() {
        let args = route_add_args("35.213.7.172", Some("192.168.88.1"), "en0");
        assert_eq!(
            args,
            vec!["-n", "add", "-inet", "35.213.7.172", "192.168.88.1", "-ifp", "en0"]
        );
    }

    #[test]
    fn route_add_args_without_gateway() {
        let args = route_add_args("10.0.0.0/8", None, "utun5");
        assert_eq!(args, vec!["-n", "add", "-inet", "10.0.0.0/8", "-interface", "utun5"]);
    }
}
