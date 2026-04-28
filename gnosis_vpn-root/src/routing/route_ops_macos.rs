//! macOS route operations using BSD route commands.
//!
//! Currently wraps the `route` command; a future iteration could use
//! PF_ROUTE sockets directly for CLI-free operation.

use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::Error;

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

/// Route table operations for macOS backed by the `route` command.
#[derive(Clone)]
pub struct DarwinRouteOps;

impl DarwinRouteOps {
    pub async fn get_default_interface(&self) -> Result<(String, Option<String>), Error> {
        let output = Command::new("route")
            .arg("-n")
            .arg("get")
            .arg("0.0.0.0")
            .run_stdout(Logs::Print)
            .await?;

        parse_route_output(&output)
    }

    pub async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error> {
        let mut cmd = Command::new("route");
        for arg in route_add_args(dest, gateway, device) {
            cmd.arg(arg);
        }
        cmd.run_stdout(Logs::Print).await?;
        Ok(())
    }

    pub async fn route_del(&self, dest: &str, _device: &str) -> Result<(), Error> {
        Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-inet")
            .arg(dest)
            .run_stdout(Logs::Suppress)
            .await?;
        Ok(())
    }
}

/// Parse `route -n get 0.0.0.0` output to extract interface name and gateway.
///
/// Handles the case where gateway shows as "index: N" (e.g. when a VPN is
/// active) by filtering out values that contain a colon suffix.
fn parse_route_output(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();

    let device = parts
        .iter()
        .position(|&x| x == "interface:")
        .and_then(|idx| parts.get(idx + 1))
        .map(|s| s.to_string())
        .ok_or_else(|| {
            tracing::error!(%output, "unable to determine default interface");
            Error::NoInterface
        })?;

    let gateway = parts
        .iter()
        .position(|&x| x == "gateway:")
        .and_then(|idx| parts.get(idx + 1))
        .filter(|gw| !gw.ends_with(':')) // filter "index:" in "gateway: index: 28"
        .map(|gw| gw.to_string());

    Ok((device, gateway))
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

    #[test]
    fn parses_interface_and_gateway() -> anyhow::Result<()> {
        let output = r#"
                  route to: default
               destination: default
                      mask: default
                   gateway: 192.168.178.1
                 interface: en1
                     flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
               "#;

        let (device, gateway) = parse_route_output(output)?;
        assert_eq!(device, "en1");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn parses_interface_no_gateway_when_vpn_active() -> anyhow::Result<()> {
        // When VPN is active, gateway may show as "index: N" instead of an IP
        let output = r#"
                             route to: default
                          destination: default
                                 mask: default
                              gateway: index: 28
                            interface: utun8
                                flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
                          "#;

        let (device, gateway) = parse_route_output(output)?;
        assert_eq!(device, "utun8");
        assert_eq!(gateway, None);
        Ok(())
    }
}
