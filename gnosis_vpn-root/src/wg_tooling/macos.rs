//! Native macOS WireGuard bring-up via `wireguard-go` and `wg setconf`.
//!
//! Mirrors what `wg-quick up` did for our `Table = off` configs:
//! IPv6 blackholes → spawn wireguard-go → resolve utun name → `wg setconf` →
//! address → MTU + up → DNS. `down` is the reverse.
//!
//! DNS is applied per network service via `networksetup`; the previous
//! servers are saved to a file in the cache dir so they survive a daemon
//! restart and can be restored on `down`. Known regression vs wg-quick:
//! there is no monitor re-applying DNS when the system pushes new settings;
//! the `wan_changed()` reconnect covers most of those cases.

use tokio::fs;
use tokio::process::Command;
use tokio::time::{Duration, sleep};

use std::collections::HashMap;
use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::wireguard::{WG_INTERFACE, WG_MTU};
use gnosis_vpn_lib::{dirs, event};

use super::{Error, parse_address, write_setconf_file};

/// IPv6 is not supported yet: blackhole both halves of the address space
/// to prevent traffic from leaking around the IPv4-only tunnel. Splitting
/// the range makes the routes more specific than router-pushed rules.
const IPV6_BLACKHOLE_NETS: &[&str] = &["::/1", "8000::/1"];

/// wireguard-go runtime directory holding the `<iface>.name` and `<utun>.sock` files.
const WIREGUARD_RUN_DIR: &str = "/var/run/wireguard";

/// Backup of per-service DNS servers taken before overwriting them.
const DNS_BACKUP_FILE: &str = "dns_backup.json";

/// Bring up the WireGuard interface. Returns the resolved utun name.
pub async fn up(state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error> {
    add_ipv6_blackholes().await?;

    match bring_up_interface(state_home, wg_data).await {
        Ok(utun) => Ok(utun),
        Err(e) => {
            // Stop a possibly running wireguard-go and undo the blackholes.
            let _ = shutdown_wireguard_go().await;
            remove_ipv6_blackholes().await;
            Err(e)
        }
    }
}

/// Tear down the WireGuard interface, restore DNS and remove the IPv6 blackholes.
pub async fn down(state_home: PathBuf, _logs: Logs) -> Result<(), Error> {
    restore_dns(state_home).await;
    let res = shutdown_wireguard_go().await;
    remove_ipv6_blackholes().await;
    res
}

async fn bring_up_interface(state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error> {
    fs::create_dir_all(WIREGUARD_RUN_DIR).await?;
    let _ = fs::remove_file(name_file()).await;

    // wireguard-go daemonizes itself once the utun device exists and writes
    // the kernel-assigned name into WG_TUN_NAME_FILE.
    Command::new("wireguard-go")
        .env("WG_TUN_NAME_FILE", name_file())
        .arg("utun")
        .run(Logs::Print)
        .await?;
    let utun = read_utun_name().await?;

    let conf_file = write_setconf_file(state_home.clone(), wg_data).await?;
    Command::new("wg")
        .arg("setconf")
        .arg(&utun)
        .arg(conf_file)
        .run(Logs::Print)
        .await?;

    let (addr, prefix) = parse_address(&wg_data.interface_info.address)?;
    let mut ifconfig_addr = Command::new("ifconfig");
    for arg in ifconfig_address_args(&utun, &addr.to_string(), prefix) {
        ifconfig_addr.arg(arg);
    }
    ifconfig_addr.run(Logs::Print).await?;

    let mtu = WG_MTU.to_string();
    Command::new("ifconfig")
        .args([utun.as_str(), "mtu", mtu.as_str(), "up"])
        .run(Logs::Print)
        .await?;

    if let Some(dns) = &wg_data.wg.config.dns {
        apply_dns(state_home, dns).await?;
    }
    Ok(utun)
}

/// Build the `ifconfig` arguments assigning the tunnel address.
/// WireGuard is point-to-point: the address doubles as the peer destination.
fn ifconfig_address_args(utun: &str, addr: &str, prefix: u8) -> Vec<String> {
    vec![
        utun.into(),
        "inet".into(),
        format!("{addr}/{prefix}"),
        addr.into(),
        "alias".into(),
    ]
}

/// Stop wireguard-go by removing its control socket, then wait for the utun
/// device to disappear so callers can safely clean up routes afterwards.
async fn shutdown_wireguard_go() -> Result<(), Error> {
    // Read once without polling: a missing name file means wireguard-go
    // is not running (or never came up), so there is nothing to stop.
    let Ok(content) = fs::read_to_string(name_file()).await else {
        return Ok(());
    };
    let Some(utun) = parse_utun_name(&content) else {
        return Ok(());
    };

    // Removing the socket makes wireguard-go exit (same mechanism wg-quick uses).
    // A missing socket means the daemon already died; the poll below confirms.
    let _ = fs::remove_file(format!("{WIREGUARD_RUN_DIR}/{utun}.sock")).await;
    let _ = fs::remove_file(name_file()).await;

    for _ in 0..50 {
        let gone = Command::new("ifconfig").arg(&utun).run(Logs::Suppress).await.is_err();
        if gone {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(Error::General(format!(
        "interface {utun} still present after wireguard-go shutdown"
    )))
}

fn name_file() -> String {
    format!("{WIREGUARD_RUN_DIR}/{WG_INTERFACE}.name")
}

/// Read the kernel-assigned utun name wireguard-go wrote to the name file.
/// Polls briefly since wireguard-go writes it asynchronously to daemonizing.
async fn read_utun_name() -> Result<String, Error> {
    for _ in 0..20 {
        if let Ok(content) = fs::read_to_string(name_file()).await {
            if let Some(name) = parse_utun_name(&content) {
                return Ok(name);
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(Error::General(format!(
        "could not resolve utun interface name from {}",
        name_file()
    )))
}

fn parse_utun_name(content: &str) -> Option<String> {
    let name = content.trim();
    if name.is_empty() { None } else { Some(name.to_string()) }
}

async fn add_ipv6_blackholes() -> Result<(), Error> {
    for net in IPV6_BLACKHOLE_NETS {
        // Delete-then-add keeps this idempotent across unclean shutdowns.
        let _ = Command::new("route")
            .args(blackhole_args("delete", net))
            .run(Logs::Suppress)
            .await;
        if let Err(e) = Command::new("route")
            .args(blackhole_args("add", net))
            .run(Logs::Print)
            .await
        {
            remove_ipv6_blackholes().await;
            return Err(e.into());
        }
    }
    Ok(())
}

async fn remove_ipv6_blackholes() {
    for net in IPV6_BLACKHOLE_NETS {
        if let Err(e) = Command::new("route")
            .args(blackhole_args("delete", net))
            .run(Logs::Suppress)
            .await
        {
            tracing::warn!(%e, net = %net, "failed to remove IPv6 blackhole route");
        }
    }
}

fn blackhole_args(action: &str, net: &str) -> Vec<String> {
    vec![
        "-n".into(),
        action.into(),
        "-blackhole".into(),
        "-inet6".into(),
        net.into(),
        "::1".into(),
    ]
}

// ============================================================================
// DNS via networksetup
// ============================================================================

/// Overwrite DNS servers on every enabled network service, saving the
/// previous servers to the cache dir first so `down` can restore them.
async fn apply_dns(state_home: PathBuf, dns: &str) -> Result<(), Error> {
    let services_out = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .run_stdout(Logs::Print)
        .await?;
    let services = parse_network_services(&services_out);

    let mut backup: HashMap<String, Vec<String>> = HashMap::new();
    for service in &services {
        let out = Command::new("networksetup")
            .args(["-getdnsservers", service])
            .run_stdout(Logs::Suppress)
            .await?;
        backup.insert(service.clone(), parse_dns_servers(&out));
    }
    let json = serde_json::to_string(&backup).map_err(|e| Error::General(format!("dns backup: {e}")))?;
    fs::write(dns_backup_file(state_home), json).await?;

    let servers: Vec<&str> = dns.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    for service in &services {
        Command::new("networksetup")
            .arg("-setdnsservers")
            .arg(service)
            .args(&servers)
            .run(Logs::Print)
            .await?;
    }
    Ok(())
}

/// Restore per-service DNS servers from the backup file; best-effort.
async fn restore_dns(state_home: PathBuf) {
    let backup_file = dns_backup_file(state_home);
    let Ok(json) = fs::read_to_string(&backup_file).await else {
        // No backup — DNS was never overwritten.
        return;
    };
    let backup: HashMap<String, Vec<String>> = match serde_json::from_str(&json) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%e, "unreadable DNS backup, leaving DNS settings as-is");
            return;
        }
    };

    for (service, servers) in backup {
        let mut cmd = Command::new("networksetup");
        cmd.arg("-setdnsservers").arg(&service);
        if servers.is_empty() {
            // networksetup's magic word for "no DNS servers configured"
            cmd.arg("empty");
        } else {
            cmd.args(&servers);
        }
        if let Err(e) = cmd.run(Logs::Print).await {
            tracing::warn!(%e, service = %service, "failed to restore DNS servers");
        }
    }
    let _ = fs::remove_file(backup_file).await;
}

fn dns_backup_file(state_home: PathBuf) -> PathBuf {
    dirs::cache_dir(state_home, DNS_BACKUP_FILE)
}

/// Parse `networksetup -listallnetworkservices` output: skip the explanatory
/// header line and disabled services (prefixed with `*`).
fn parse_network_services(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('*'))
        .map(str::to_string)
        .collect()
}

/// Parse `networksetup -getdnsservers <service>` output. When no servers are
/// configured the command prints an explanatory sentence instead of addresses.
fn parse_dns_servers(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifconfig_address_args_are_point_to_point() {
        let args = ifconfig_address_args("utun5", "10.128.0.5", 32);
        assert_eq!(args, vec!["utun5", "inet", "10.128.0.5/32", "10.128.0.5", "alias"]);
    }

    #[test]
    fn blackhole_args_split_ipv6_range() {
        assert_eq!(
            blackhole_args("add", "::/1"),
            vec!["-n", "add", "-blackhole", "-inet6", "::/1", "::1"]
        );
        assert_eq!(
            blackhole_args("delete", "8000::/1"),
            vec!["-n", "delete", "-blackhole", "-inet6", "8000::/1", "::1"]
        );
    }

    #[test]
    fn parse_utun_name_trims_and_rejects_empty() {
        assert_eq!(parse_utun_name("utun5\n"), Some("utun5".to_string()));
        assert_eq!(parse_utun_name("  \n"), None);
    }

    #[test]
    fn parse_network_services_skips_header_and_disabled() {
        let output = "\
An asterisk (*) denotes that a network service is disabled.
Wi-Fi
*Thunderbolt Bridge
USB 10/100/1000 LAN
";
        assert_eq!(parse_network_services(output), vec!["Wi-Fi", "USB 10/100/1000 LAN"]);
    }

    #[test]
    fn parse_dns_servers_reads_addresses() {
        assert_eq!(
            parse_dns_servers("192.168.1.1\nfd00::1\n"),
            vec!["192.168.1.1", "fd00::1"]
        );
    }

    #[test]
    fn parse_dns_servers_treats_message_as_no_servers() {
        let output = "There aren't any DNS Servers set on Wi-Fi.\n";
        assert!(parse_dns_servers(output).is_empty());
    }
}
