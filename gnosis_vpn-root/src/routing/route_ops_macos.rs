//! macOS route operations using BSD route commands.
//!
//! [`DarwinRouteOps`] implements [`RouteOps`] using macOS-native routing.
//! Currently wraps the `route` command; a future iteration could use
//! PF_ROUTE sockets directly for CLI-free operation.

use async_trait::async_trait;
use std::net::Ipv4Addr;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::Error;
use super::route_ops::{RouteOps, WanRoute};

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
        parse_key_value_output(&output, "interface:", "gateway:", Some(":"))
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

    async fn get_wan_route_for(&self, _dest: Ipv4Addr, exclude_iface: &str) -> Result<Option<WanRoute>, Error> {
        let output = Command::new("netstat")
            .arg("-rn")
            .arg("-f")
            .arg("inet")
            .run_stdout(Logs::Suppress)
            .await?;

        let (device, gateway) = match parse_netstat_default_excluding(&output, exclude_iface) {
            Ok(pair) => pair,
            Err(_) => return Ok(None),
        };

        let src_ip = get_interface_address(&device).await;

        Ok(Some(WanRoute {
            device,
            gateway,
            src_ip,
        }))
    }
}

/// Parses key-value pairs from command output to extract device and gateway.
///
/// This utility works for both Linux (`ip route show default`) and macOS
/// (`route -n get 0.0.0.0`) command outputs by parameterizing the key names.
///
/// # Arguments
/// * `output` - The command output to parse
/// * `device_key` - Key for device name (e.g., "dev" on Linux, "interface:" on macOS)
/// * `gateway_key` - Key for gateway IP (e.g., "via" on Linux, "gateway:" on macOS)
/// * `filter_suffix` - Optional suffix to filter out (e.g., Some(":") for macOS
///   to handle "gateway: index: 28" cases)
///
/// # Returns
/// A tuple of (device_name, Option<gateway_ip>)
pub(crate) fn parse_key_value_output(
    output: &str,
    device_key: &str,
    gateway_key: &str,
    filter_suffix: Option<&str>,
) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();

    let device_index = parts.iter().position(|&x| x == device_key);
    let gateway_index = parts.iter().position(|&x| x == gateway_key);

    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = gateway_index
        .and_then(|idx| parts.get(idx + 1))
        .filter(|gw| {
            // Filter out values matching the suffix (e.g., "index:" on macOS)
            filter_suffix.is_none_or(|suffix| !gw.ends_with(suffix))
        })
        .map(|gw| gw.to_string());

    Ok((device, gateway))
}

/// Parses `netstat -rn -f inet` output, returning device and gateway of the
/// WAN default route — the entry whose destination is exactly `default`.
///
/// Unlike `route get 0.0.0.0` (longest-prefix match), the `/0` entry stays
/// visible in the routing table while our VPN split routes (`0.0.0.0/1`,
/// `128.0.0.0/1`) are installed, so this lookup is reliable mid-connection.
pub(crate) fn parse_netstat_default(output: &str) -> Result<(String, Option<String>), Error> {
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [dest, gateway, flags, netif, ..] = tokens[..] else {
            continue;
        };
        if dest != "default" {
            continue;
        }
        // 'I' flags an interface-scoped default route, not the primary WAN default
        if flags.contains('I') {
            continue;
        }
        // a link-level gateway ("link#N") is not an IP gateway
        let gateway = if gateway.starts_with("link#") {
            None
        } else {
            Some(gateway.to_string())
        };
        return Ok((netif.to_string(), gateway));
    }
    tracing::error!(%output, "Unable to determine WAN default route from netstat");
    Err(Error::NoInterface)
}

/// Like [`parse_netstat_default`] but skips entries whose `netif` matches `exclude_iface`.
/// Used by `get_wan_route_for` to skip the VPN tunnel interface.
fn parse_netstat_default_excluding(output: &str, exclude_iface: &str) -> Result<(String, Option<String>), Error> {
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [dest, gateway, flags, netif, ..] = tokens[..] else {
            continue;
        };
        if dest != "default" {
            continue;
        }
        if flags.contains('I') {
            continue;
        }
        if netif == exclude_iface {
            continue;
        }
        let gateway = if gateway.starts_with("link#") {
            None
        } else {
            Some(gateway.to_string())
        };
        return Ok((netif.to_string(), gateway));
    }
    tracing::error!(%output, "Unable to determine WAN default route from netstat (excluding {exclude_iface})");
    Err(Error::NoInterface)
}

/// Returns the first IPv4 address assigned to `device` via `ifconfig`.
async fn get_interface_address(device: &str) -> Option<Ipv4Addr> {
    let output = Command::new("ifconfig")
        .arg(device)
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first() == Some(&"inet") {
            return tokens.get(1).and_then(|s| s.parse().ok());
        }
    }
    None
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
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = r#"
                      route to: default
                   destination: default
                          mask: default
                       gateway: 192.168.178.1
                     interface: en1
                         flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
                    recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
                          0         0         0         0         0         0      1500         0
                   "#;

        let (device, gateway) = super::parse_key_value_output(output, "interface:", "gateway:", Some(":"))?;

        assert_eq!(device, "en1");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn netstat_default_plain() -> anyhow::Result<()> {
        let output = "Routing tables\n\n\
                      Internet:\n\
                      Destination        Gateway            Flags           Netif Expire\n\
                      default            192.168.178.1      UGScg             en0\n\
                      127                127.0.0.1          UCS               lo0\n";
        let (device, gateway) = parse_netstat_default(output)?;
        assert_eq!(device, "en0");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn netstat_default_link_gateway_maps_to_none() -> anyhow::Result<()> {
        let output = "Destination        Gateway            Flags           Netif Expire\n\
                      default            link#28            UCSg            utun8\n";
        let (device, gateway) = parse_netstat_default(output)?;
        assert_eq!(device, "utun8");
        assert_eq!(gateway, None);
        Ok(())
    }

    #[test]
    fn netstat_default_skips_ifscoped_entry() -> anyhow::Result<()> {
        let output = "Destination        Gateway            Flags           Netif Expire\n\
                      default            192.168.1.1        UGScIg            en1\n\
                      default            192.168.178.1      UGScg             en0\n";
        let (device, gateway) = parse_netstat_default(output)?;
        assert_eq!(device, "en0");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn netstat_default_ignores_vpn_split_routes() -> anyhow::Result<()> {
        // While connected, our split routes (0/1, 128.0/1) and VPN subnet are
        // present — the /0 "default" entry must still resolve to the WAN.
        let output = "Destination        Gateway            Flags           Netif Expire\n\
                      0/1                utun8              USc             utun8\n\
                      default            192.168.178.1      UGScg             en0\n\
                      10.128/9           utun8              USc             utun8\n\
                      128.0/1            utun8              USc             utun8\n";
        let (device, gateway) = parse_netstat_default(output)?;
        assert_eq!(device, "en0");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn netstat_default_no_default_route_errors() {
        let output = "Destination        Gateway            Flags           Netif Expire\n\
                      127                127.0.0.1          UCS               lo0\n";
        assert!(parse_netstat_default(output).is_err());
    }

    #[test]
    fn parses_interface_no_gateway_with_index() -> anyhow::Result<()> {
        // When VPN is active, gateway may show as "index: N" instead of an IP
        let output = r#"
                                 route to: default
                              destination: default
                                     mask: default
                                  gateway: index: 28
                                interface: utun8
                                    flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
                              "#;

        let (device, gateway) = super::parse_key_value_output(output, "interface:", "gateway:", Some(":"))?;

        assert_eq!(device, "utun8");
        assert_eq!(gateway, None); // Should be None, not "index:"
        Ok(())
    }
}
