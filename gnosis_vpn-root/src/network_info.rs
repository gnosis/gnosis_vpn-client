//! Collects host network state at daemon startup for diagnostics.
//!
//! Knowing the routing setup before the VPN starts makes user issues much
//! easier to diagnose. All errors are non-fatal — unavailable fields show as "?".

use std::fmt;
use tokio::fs;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

// ============================================================================
// Public Types
// ============================================================================

pub struct NetworkInfo {
    pub ipv4_route: Option<RouteInfo>,
    pub ipv6_route: Option<RouteInfo>,
    pub dns_nameservers: Vec<String>,
}

pub struct RouteInfo {
    pub interface: String,
    pub gateway: Option<String>,
    pub ip: Option<String>,
}

impl NetworkInfo {
    pub async fn gather() -> Self {
        let ipv4_route = gather_ipv4_route().await;
        let ipv6_route = gather_ipv6_route().await;
        let dns_nameservers = gather_dns_nameservers().await;
        Self {
            ipv4_route,
            ipv6_route,
            dns_nameservers,
        }
    }
}

impl fmt::Display for NetworkInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ipv4_default_route={}", format_route(&self.ipv4_route))?;
        write!(f, " ipv6_default_route={}", format_route(&self.ipv6_route))?;
        write!(f, " dns={}", format_dns(&self.dns_nameservers))
    }
}

fn format_route(route: &Option<RouteInfo>) -> String {
    match route {
        None => "none".to_string(),
        Some(r) => format!(
            "(interface={} gateway={} ip={})",
            r.interface,
            r.gateway.as_deref().unwrap_or("?"),
            r.ip.as_deref().unwrap_or("?"),
        ),
    }
}

fn format_dns(servers: &[String]) -> String {
    if servers.is_empty() {
        "none".to_string()
    } else {
        servers.join(",")
    }
}

// ============================================================================
// Gathering — Linux
// ============================================================================

#[cfg(target_os = "linux")]
async fn gather_ipv4_route() -> Option<RouteInfo> {
    let route_output = Command::new("ip")
        .args(["-4", "route", "show", "default"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;

    let mut route = parse_ipv4_linux_route(&route_output)?;

    if route.ip.is_none() {
        let addr_output = Command::new("ip")
            .args(["-4", "addr", "show", "dev", &route.interface])
            .run_stdout(Logs::Suppress)
            .await
            .ok()?;
        route.ip = parse_ipv4_linux_addr(&addr_output);
    }

    Some(route)
}

#[cfg(target_os = "linux")]
async fn gather_ipv6_route() -> Option<RouteInfo> {
    let route_output = Command::new("ip")
        .args(["-6", "route", "show", "default"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;

    let mut route = parse_ipv6_linux_route(&route_output)?;

    let addr_output = Command::new("ip")
        .args(["-6", "addr", "show", "dev", &route.interface, "scope", "global"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;
    route.ip = parse_ipv6_linux_addr(&addr_output);

    Some(route)
}

// ============================================================================
// Gathering — macOS
// ============================================================================

#[cfg(target_os = "macos")]
async fn gather_ipv4_route() -> Option<RouteInfo> {
    let route_output = Command::new("route")
        .args(["-n", "get", "0.0.0.0"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;

    let mut route = parse_ipv4_macos_route(&route_output)?;

    let addr_output = Command::new("ifconfig")
        .args([&route.interface])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;
    route.ip = parse_ipv4_macos_addr(&addr_output);

    Some(route)
}

#[cfg(target_os = "macos")]
async fn gather_ipv6_route() -> Option<RouteInfo> {
    let route_output = Command::new("route")
        .args(["-n", "get", "-inet6", "::"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;

    let mut route = parse_ipv6_macos_route(&route_output)?;

    let addr_output = Command::new("ifconfig")
        .args([&route.interface, "inet6"])
        .run_stdout(Logs::Suppress)
        .await
        .ok()?;
    route.ip = parse_ipv6_macos_addr(&addr_output);

    Some(route)
}

// ============================================================================
// DNS — all platforms
// ============================================================================

async fn gather_dns_nameservers() -> Vec<String> {
    match fs::read_to_string("/etc/resolv.conf").await {
        Ok(contents) => parse_dns_nameservers(&contents),
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Parsers (pure — testable without I/O)
// ============================================================================

/// Parses `ip -4 route show default` output.
/// Takes the first default route (lowest metric wins in practice).
#[cfg(target_os = "linux")]
fn parse_ipv4_linux_route(output: &str) -> Option<RouteInfo> {
    let line = output.lines().find(|l| l.starts_with("default"))?;
    let tokens: Vec<&str> = line.split_whitespace().collect();

    let interface = after(&tokens, "dev").map(String::from)?;
    let gateway = after(&tokens, "via").map(String::from);
    // "src" is the kernel's preferred source address for this route.
    let ip = after(&tokens, "src").map(String::from);

    Some(RouteInfo { interface, gateway, ip })
}

/// Parses `ip -4 addr show dev <iface>` output, returning the first inet address.
#[cfg(target_os = "linux")]
fn parse_ipv4_linux_addr(output: &str) -> Option<String> {
    // "    inet 192.168.1.100/24 brd ... scope global ..."
    output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("inet "))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|cidr| cidr.split('/').next())
        .map(String::from)
}

/// Parses `ip -6 route show default` output.
#[cfg(target_os = "linux")]
fn parse_ipv6_linux_route(output: &str) -> Option<RouteInfo> {
    let line = output.lines().find(|l| l.starts_with("default"))?;
    let tokens: Vec<&str> = line.split_whitespace().collect();

    let interface = after(&tokens, "dev").map(String::from)?;
    let gateway = after(&tokens, "via").map(String::from);

    Some(RouteInfo {
        interface,
        gateway,
        ip: None,
    })
}

/// Parses `ip -6 addr show dev <iface> scope global` output.
#[cfg(target_os = "linux")]
fn parse_ipv6_linux_addr(output: &str) -> Option<String> {
    // "    inet6 2001:db8::1/64 scope global ..."
    output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("inet6 "))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|cidr| cidr.split('/').next())
        .map(String::from)
}

/// Parses `route -n get 0.0.0.0` output on macOS.
#[cfg(target_os = "macos")]
fn parse_ipv4_macos_route(output: &str) -> Option<RouteInfo> {
    let interface = kv_field(output, "interface:")?;
    let gateway = kv_field(output, "gateway:");
    Some(RouteInfo {
        interface,
        gateway,
        ip: None,
    })
}

/// Parses `ifconfig <iface>` output on macOS, returning the first inet address.
#[cfg(target_os = "macos")]
fn parse_ipv4_macos_addr(output: &str) -> Option<String> {
    // "\tinet 192.168.1.100 netmask ..."
    output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("inet ") && !l.starts_with("inet6"))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(String::from)
}

/// Parses `route -n get -inet6 ::` output on macOS.
#[cfg(target_os = "macos")]
fn parse_ipv6_macos_route(output: &str) -> Option<RouteInfo> {
    let interface = kv_field(output, "interface:")?;
    let gateway = kv_field(output, "gateway:");
    Some(RouteInfo {
        interface,
        gateway,
        ip: None,
    })
}

/// Parses `ifconfig <iface> inet6` output on macOS.
/// Skips link-local addresses (they contain '%' for the scope identifier).
#[cfg(target_os = "macos")]
fn parse_ipv6_macos_addr(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("inet6 "))
        .filter_map(|l| l.split_whitespace().nth(1))
        .find(|addr| !addr.contains('%'))
        .map(String::from)
}

/// Parses `/etc/resolv.conf` content, extracting nameserver addresses.
fn parse_dns_nameservers(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("nameserver "))
        .filter_map(|s| s.split_whitespace().next())
        .map(String::from)
        .collect()
}

// ============================================================================
// Helpers
// ============================================================================

/// Returns the token immediately after `keyword` in a whitespace-split line.
fn after<'a>(tokens: &[&'a str], keyword: &str) -> Option<&'a str> {
    tokens.windows(2).find(|w| w[0] == keyword).map(|w| w[1])
}

/// Extracts a value from a `route` output line formatted as `"  key: value"`.
#[cfg(target_os = "macos")]
fn kv_field(output: &str, key: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(String::from)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- Linux IPv4 route ---

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_route_with_src() {
        let output = "default via 192.168.1.1 dev eth0 proto dhcp src 192.168.1.100 metric 100\n";
        let route = parse_ipv4_linux_route(output).unwrap();
        assert_eq!(route.interface, "eth0");
        assert_eq!(route.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(route.ip.as_deref(), Some("192.168.1.100"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_route_without_src() {
        let output = "default via 10.0.0.1 dev wlan0 proto dhcp metric 600\n";
        let route = parse_ipv4_linux_route(output).unwrap();
        assert_eq!(route.interface, "wlan0");
        assert_eq!(route.gateway.as_deref(), Some("10.0.0.1"));
        assert!(route.ip.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_route_empty_output() {
        assert!(parse_ipv4_linux_route("").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_route_picks_first_default() {
        let output = "default via 192.168.1.1 dev eth0 metric 100\n\
                      default via 10.0.0.1 dev wlan0 metric 600\n";
        let route = parse_ipv4_linux_route(output).unwrap();
        assert_eq!(route.interface, "eth0");
    }

    // --- Linux IPv4 addr ---

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_addr_parses_cidr() {
        let output = "2: eth0: <BROADCAST,MULTICAST,UP> mtu 1500\n\
                      \tlink/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff\n\
                      \tinet 192.168.1.100/24 brd 192.168.1.255 scope global dynamic eth0\n";
        assert_eq!(parse_ipv4_linux_addr(output).as_deref(), Some("192.168.1.100"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv4_linux_addr_empty_output() {
        assert!(parse_ipv4_linux_addr("").is_none());
    }

    // --- Linux IPv6 route ---

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv6_linux_route_with_gateway() {
        let output = "default via fe80::1 dev eth0 proto ra metric 100 expires 1799sec\n";
        let route = parse_ipv6_linux_route(output).unwrap();
        assert_eq!(route.interface, "eth0");
        assert_eq!(route.gateway.as_deref(), Some("fe80::1"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv6_linux_route_empty_output() {
        assert!(parse_ipv6_linux_route("").is_none());
    }

    // --- Linux IPv6 addr ---

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv6_linux_addr_strips_prefix_len() {
        let output = "2: eth0: <BROADCAST,MULTICAST,UP>\n\
                      \tinet6 2001:db8::1/64 scope global dynamic mngtmpaddr\n\
                      \tinet6 fe80::1/64 scope link\n";
        assert_eq!(parse_ipv6_linux_addr(output).as_deref(), Some("2001:db8::1"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ipv6_linux_addr_empty_output() {
        assert!(parse_ipv6_linux_addr("").is_none());
    }

    // --- macOS IPv4 route ---

    #[cfg(target_os = "macos")]
    #[test]
    fn ipv4_macos_route_parses_fields() {
        let output = "   route to: default\n\
                      destination: default\n\
                          gateway: 192.168.1.1\n\
                        interface: en0\n";
        let route = parse_ipv4_macos_route(output).unwrap();
        assert_eq!(route.interface, "en0");
        assert_eq!(route.gateway.as_deref(), Some("192.168.1.1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ipv4_macos_route_missing_interface_returns_none() {
        let output = "   route to: default\n    gateway: 192.168.1.1\n";
        assert!(parse_ipv4_macos_route(output).is_none());
    }

    // --- macOS IPv4 addr ---

    #[cfg(target_os = "macos")]
    #[test]
    fn ipv4_macos_addr_parses_inet() {
        let output = "en0: flags=...\n\
                      \tinet6 fe80::1%en0 prefixlen 64\n\
                      \tinet 192.168.1.100 netmask 0xffffff00 broadcast 192.168.1.255\n";
        assert_eq!(parse_ipv4_macos_addr(output).as_deref(), Some("192.168.1.100"));
    }

    // --- macOS IPv6 addr ---

    #[cfg(target_os = "macos")]
    #[test]
    fn ipv6_macos_addr_skips_link_local() {
        let output = "en0: flags=...\n\
                      \tinet6 fe80::1%en0 prefixlen 64\n\
                      \tinet6 2001:db8::1 prefixlen 64 autoconf secured\n";
        assert_eq!(parse_ipv6_macos_addr(output).as_deref(), Some("2001:db8::1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ipv6_macos_addr_only_link_local_returns_none() {
        let output = "en0: flags=...\n\tinet6 fe80::1%en0 prefixlen 64\n";
        assert!(parse_ipv6_macos_addr(output).is_none());
    }

    // --- DNS ---

    #[test]
    fn dns_parses_nameserver_lines() {
        let content = "# Generated by NetworkManager\nnameserver 8.8.8.8\nnameserver 8.8.4.4\nsearch example.com\n";
        assert_eq!(parse_dns_nameservers(content), vec!["8.8.8.8", "8.8.4.4"]);
    }

    #[test]
    fn dns_empty_file_returns_empty() {
        assert!(parse_dns_nameservers("").is_empty());
    }

    #[test]
    fn dns_only_comments_returns_empty() {
        assert!(parse_dns_nameservers("# nameserver 1.1.1.1\n").is_empty());
    }

    #[test]
    fn dns_strips_inline_comments() {
        let content = "nameserver 8.8.8.8 # corp-dns\nnameserver 1.1.1.1\n";
        assert_eq!(parse_dns_nameservers(content), vec!["8.8.8.8", "1.1.1.1"]);
    }

    #[test]
    fn dns_handles_leading_whitespace() {
        let content = "  nameserver 1.1.1.1\n\tnameserver 8.8.8.8\n";
        assert_eq!(parse_dns_nameservers(content), vec!["1.1.1.1", "8.8.8.8"]);
    }

    // --- Display ---

    #[test]
    fn display_with_all_info() {
        let info = NetworkInfo {
            ipv4_route: Some(RouteInfo {
                interface: "eth0".to_string(),
                gateway: Some("192.168.1.1".to_string()),
                ip: Some("192.168.1.100".to_string()),
            }),
            ipv6_route: Some(RouteInfo {
                interface: "eth0".to_string(),
                gateway: Some("fe80::1".to_string()),
                ip: Some("2001:db8::1".to_string()),
            }),
            dns_nameservers: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
        };
        assert_eq!(
            info.to_string(),
            "ipv4_default_route=(interface=eth0 gateway=192.168.1.1 ip=192.168.1.100) \
             ipv6_default_route=(interface=eth0 gateway=fe80::1 ip=2001:db8::1) \
             dns=8.8.8.8,8.8.4.4"
        );
    }

    #[test]
    fn display_with_no_routes() {
        let info = NetworkInfo {
            ipv4_route: None,
            ipv6_route: None,
            dns_nameservers: Vec::new(),
        };
        assert_eq!(
            info.to_string(),
            "ipv4_default_route=none ipv6_default_route=none dns=none"
        );
    }

    #[test]
    fn display_with_missing_optional_fields() {
        let info = NetworkInfo {
            ipv4_route: Some(RouteInfo {
                interface: "eth0".to_string(),
                gateway: None,
                ip: None,
            }),
            ipv6_route: None,
            dns_nameservers: vec!["1.1.1.1".to_string()],
        };
        assert_eq!(
            info.to_string(),
            "ipv4_default_route=(interface=eth0 gateway=? ip=?) ipv6_default_route=none dns=1.1.1.1"
        );
    }
}
