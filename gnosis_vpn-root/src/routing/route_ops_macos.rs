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
pub struct DarwinRouteOps;

#[async_trait]
impl RouteOps for DarwinRouteOps {
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

    async fn get_route_via_device(&self, _dest: Ipv4Addr, device: &str) -> Result<Option<WanRoute>, Error> {
        // Use netstat rather than `route get -ifscope` because the VPN split routes
        // (0/1, 128/1) shadow the scoped FIB lookup for public destinations while
        // the tunnel is up, causing the command to error and falsely appear as if
        // the WAN device has no route. netstat shows the raw routing table entries
        // (the 0/0 default route via the WAN interface is always present alongside
        // the more-specific VPN routes) and is unaffected by route shadowing.
        let output = Command::new("netstat")
            .args(["-rn", "-f", "inet"])
            .run_stdout(Logs::Suppress)
            .await?;

        let Some(gateway) = parse_netstat_default_for_device(&output, device) else {
            return Ok(None);
        };

        let src_ip = get_interface_address(device).await;

        Ok(Some(WanRoute {
            device: device.to_owned(),
            gateway,
            src_ip,
        }))
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

/// Parses `netstat -rn -f inet` output, returning the gateway for entries whose `netif` matches `device`.
/// Returns `Some(gateway)` when the device has a default route, `None` when it does not.
/// Used by `get_route_via_device` to check whether the captured WAN device is still viable.
fn parse_netstat_default_for_device(output: &str, device: &str) -> Option<Option<String>> {
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [dest, gateway, _, netif, ..] = tokens[..] else {
            continue;
        };
        if dest != "default" || netif != device {
            continue;
        }
        // Do NOT filter out 'I'-scoped routes here. When a higher-priority interface
        // is added (e.g. cable while WiFi VPN is up), macOS demotes the original
        // interface's default route to interface-scoped ('I' flag) but the interface
        // is still connected and its bypass routes remain valid — we should not reconnect.
        let gateway = if gateway.starts_with("link#") {
            None
        } else {
            Some(gateway.to_string())
        };
        return Some(gateway);
    }
    None
}

/// Parses `netstat -rn -f inet` output, returning device and gateway of the WAN default route,
/// skipping entries whose `netif` matches `exclude_iface`. Used by `get_wan_route_for` to skip the VPN tunnel interface.
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

    // Realistic `netstat -rn -f inet` header + rows used across parser tests.
    const NETSTAT_OUTPUT: &str = "\
Routing tables

Internet:
Destination        Gateway            Flags               Netif Expire
default            192.168.1.1        UGScg               en0
default            link#18            UCSIg               utun5
default            10.0.0.1           UGSc                en1
127                127.0.0.1          UCS                 lo0
";

    // ── parse_netstat_default_for_device ────────────────────────────────────

    #[test]
    fn netstat_for_device_returns_gateway_when_found() {
        let result = parse_netstat_default_for_device(NETSTAT_OUTPUT, "en0");
        assert_eq!(result, Some(Some("192.168.1.1".to_string())));
    }

    #[test]
    fn netstat_for_device_returns_none_gateway_for_link_local() {
        // utun5 row has `link#18` as gateway → no routable gateway.
        let result = parse_netstat_default_for_device(NETSTAT_OUTPUT, "utun5");
        assert_eq!(result, Some(None));
    }

    #[test]
    fn netstat_for_device_returns_none_when_device_absent() {
        let result = parse_netstat_default_for_device(NETSTAT_OUTPUT, "en9");
        assert_eq!(result, None);
    }

    #[test]
    fn netstat_for_device_accepts_interface_scoped_route() {
        // 'I'-flagged routes must NOT be filtered out for this parser — the WAN
        // interface is still valid when macOS demotes its route to interface-scoped.
        let output = "default            192.168.2.1        UGScgI              en0\n";
        let result = parse_netstat_default_for_device(output, "en0");
        assert_eq!(result, Some(Some("192.168.2.1".to_string())));
    }

    #[test]
    fn netstat_for_device_skips_header_lines() {
        let result = parse_netstat_default_for_device(NETSTAT_OUTPUT, "Destination");
        assert_eq!(result, None);
    }

    // ── parse_netstat_default_excluding ─────────────────────────────────────

    #[test]
    fn netstat_excluding_returns_first_non_excluded_route() {
        // utun5 is the VPN tunnel; en0 should be returned as the WAN default.
        let result = parse_netstat_default_excluding(NETSTAT_OUTPUT, "utun5");
        assert_eq!(result, Ok(("en0".to_string(), Some("192.168.1.1".to_string()))));
    }

    #[test]
    fn netstat_excluding_skips_excluded_iface() {
        // Exclude en0; next non-I default is en1.
        let result = parse_netstat_default_excluding(NETSTAT_OUTPUT, "en0");
        assert_eq!(result, Ok(("en1".to_string(), Some("10.0.0.1".to_string()))));
    }

    #[test]
    fn netstat_excluding_skips_interface_scoped_routes() {
        // utun5 row has the 'I' flag and must be skipped even when it is not the
        // excluded interface.
        let output = "\
default            link#18            UCSIg               utun5
default            192.168.1.1        UGScg               en0
";
        let result = parse_netstat_default_excluding(output, "en9");
        assert_eq!(result, Ok(("en0".to_string(), Some("192.168.1.1".to_string()))));
    }

    #[test]
    fn netstat_excluding_returns_none_gateway_for_link_local() {
        let output = "default            link#5             UCSg                en0\n";
        let result = parse_netstat_default_excluding(output, "utun5");
        assert_eq!(result, Ok(("en0".to_string(), None)));
    }

    #[test]
    fn netstat_excluding_errors_when_no_default_route_remains() {
        let result = parse_netstat_default_excluding("127  127.0.0.1  UCS  lo0\n", "en0");
        assert!(matches!(result, Err(Error::NoInterface)));
    }
}
