//! Native Linux WireGuard bring-up via rtnetlink and `wg setconf`.
//!
//! Mirrors what `wg-quick up` did for our `Table = off` configs:
//! IPv6 blackholes → create wg link → `wg setconf` → address → MTU + up → DNS.
//! `down` is the reverse.

use futures::TryStreamExt;
use rtnetlink::packet_route::route::RouteType;
use rtnetlink::{Handle, LinkUnspec, LinkWireguard, RouteMessageBuilder};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::net::{IpAddr, Ipv6Addr};
use std::path::PathBuf;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::{self, Logs, ShellCommandExt};
use gnosis_vpn_lib::wireguard::{WG_INTERFACE, WG_MTU};

use super::{Error, parse_address, write_setconf_file};

/// IPv6 is not supported yet: blackhole both halves of the address space
/// to prevent traffic from leaking around the IPv4-only tunnel.
const IPV6_BLACKHOLE_NETS: &[(Ipv6Addr, u8)] = &[
    (Ipv6Addr::UNSPECIFIED, 1),                      // ::/1
    (Ipv6Addr::new(0x8000, 0, 0, 0, 0, 0, 0, 0), 1), // 8000::/1
];

/// Bring up the WireGuard interface. Returns the interface name.
pub async fn up(handle: &Handle, state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<String, Error> {
    add_ipv6_blackholes(handle).await?;

    if let Err(e) = bring_up_interface(handle, state_home, wg_data).await {
        remove_interface(handle).await;
        remove_ipv6_blackholes(handle).await;
        return Err(e);
    }

    Ok(WG_INTERFACE.to_string())
}

/// Tear down the WireGuard interface and the IPv6 blackholes.
pub async fn down(handle: &Handle, _state_home: PathBuf, _logs: Logs) -> Result<(), Error> {
    // Unregister DNS first; best-effort since DNS may not have been configured.
    let _ = Command::new("resolvconf")
        .args(["-d", WG_INTERFACE, "-f"])
        .run(Logs::Suppress)
        .await;

    let index = resolve_ifindex(handle, WG_INTERFACE).await?;
    handle.link().del(index).execute().await?;

    remove_ipv6_blackholes(handle).await;
    Ok(())
}

async fn bring_up_interface(handle: &Handle, state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<(), Error> {
    // Remove a stale interface from a previous unclean shutdown, then create fresh.
    remove_interface(handle).await;
    handle
        .link()
        .add(LinkWireguard::new(WG_INTERFACE).build())
        .execute()
        .await?;

    let conf_file = write_setconf_file(state_home, wg_data).await?;
    Command::new("wg")
        .arg("setconf")
        .arg(WG_INTERFACE)
        .arg(conf_file)
        .run(Logs::Print)
        .await?;

    let index = resolve_ifindex(handle, WG_INTERFACE).await?;
    let (addr, prefix) = parse_address(&wg_data.interface_info.address)?;
    handle.address().add(index, IpAddr::V4(addr), prefix).execute().await?;

    handle
        .link()
        .set(LinkUnspec::new_with_index(index).mtu(WG_MTU).up().build())
        .execute()
        .await?;

    if let Some(dns) = &wg_data.wg.config.dns {
        apply_dns(dns).await?;
    }
    Ok(())
}

/// Best-effort removal of the WireGuard interface; a missing interface is fine.
async fn remove_interface(handle: &Handle) {
    let Ok(index) = resolve_ifindex(handle, WG_INTERFACE).await else {
        return;
    };
    if let Err(e) = handle.link().del(index).execute().await {
        tracing::warn!(%e, interface = WG_INTERFACE, "failed to remove WireGuard interface");
    }
}

async fn add_ipv6_blackholes(handle: &Handle) -> Result<(), Error> {
    for (net, prefix) in IPV6_BLACKHOLE_NETS {
        // Delete-then-add keeps this idempotent across unclean shutdowns.
        let _ = handle.route().del(blackhole_route(*net, *prefix)).execute().await;
        if let Err(e) = handle.route().add(blackhole_route(*net, *prefix)).execute().await {
            remove_ipv6_blackholes(handle).await;
            return Err(e.into());
        }
    }
    Ok(())
}

async fn remove_ipv6_blackholes(handle: &Handle) {
    for (net, prefix) in IPV6_BLACKHOLE_NETS {
        if let Err(e) = handle.route().del(blackhole_route(*net, *prefix)).execute().await {
            tracing::warn!(%e, net = %net, prefix = %prefix, "failed to remove IPv6 blackhole route");
        }
    }
}

fn blackhole_route(net: Ipv6Addr, prefix: u8) -> rtnetlink::packet_route::route::RouteMessage {
    RouteMessageBuilder::<Ipv6Addr>::new()
        .destination_prefix(net, prefix)
        .kind(RouteType::BlackHole)
        .build()
}

/// Register DNS servers for the wg interface, mirroring wg-quick's
/// `resolvconf -a <iface> -m 0 -x` invocation. Nameservers are passed on
/// stdin, never on the command line.
async fn apply_dns(dns: &str) -> Result<(), Error> {
    let nameservers = nameserver_lines(dns);

    let mut command = Command::new("resolvconf")
        .args(["-a", WG_INTERFACE, "-m", "0", "-x"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = command.stdin.as_mut() {
        stdin.write_all(nameservers.as_bytes()).await?;
    }

    let cmd_debug = format!("{:?}", command);
    let output = command.wait_with_output().await?;
    shell_command_ext::stdout_from_output(cmd_debug, output, Logs::Print)?;
    Ok(())
}

/// Convert the comma-separated DNS config value into resolv.conf lines.
fn nameserver_lines(dns: &str) -> String {
    dns.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|server| format!("nameserver {server}\n"))
        .collect()
}

async fn resolve_ifindex(handle: &Handle, device: &str) -> Result<u32, Error> {
    let links: Vec<_> = handle
        .link()
        .get()
        .match_name(device.to_string())
        .execute()
        .try_collect()
        .await
        .map_err(|e| Error::General(format!("failed to resolve interface '{device}': {e}")))?;

    links
        .first()
        .map(|l| l.header.index)
        .ok_or_else(|| Error::General(format!("interface '{device}' not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nameserver_lines_from_comma_separated_config() {
        assert_eq!(
            nameserver_lines("1.1.1.1,8.8.8.8"),
            "nameserver 1.1.1.1\nnameserver 8.8.8.8\n"
        );
    }

    #[test]
    fn nameserver_lines_trims_whitespace_and_skips_empty_entries() {
        assert_eq!(
            nameserver_lines(" 1.1.1.1 , ,8.8.8.8,"),
            "nameserver 1.1.1.1\nnameserver 8.8.8.8\n"
        );
    }

    #[test]
    fn blackhole_route_message_is_ipv6_blackhole() {
        let msg = blackhole_route(Ipv6Addr::UNSPECIFIED, 1);
        assert_eq!(msg.header.kind, RouteType::BlackHole);
        assert_eq!(msg.header.destination_prefix_length, 1);
    }
}
