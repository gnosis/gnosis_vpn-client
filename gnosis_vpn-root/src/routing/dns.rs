//! Best-effort DNS diversion for the NepTUN data plane: a failure to set or restore DNS never fails the connection.

use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use super::resolv_conf;

/// The resolver mechanism used to apply DNS at setup, recorded so teardown (and the
/// crash-recovery sweep after an unclean exit) reverses the matching one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mechanism {
    /// systemd-resolved via `resolvectl` (Linux).
    Resolvectl,
    /// resolvconf via `resolvconf -a`/`-d` (Linux distros without systemd-resolved).
    Resolvconf,
    /// `/etc/resolv.conf` rewritten in place with a backup (Linux hosts where no resolver manager owns it).
    StaticFile,
    /// Supplemental resolver in the dynamic store via `scutil` (macOS).
    Scutil,
}

/// Push `servers` (a comma-separated list) as the DNS resolvers scoped to the
/// tunnel interface. An empty/blank list is a no-op. Returns the mechanism that
/// took effect so [`restore`] can reverse it, or `None` if nothing was applied.
pub async fn set(interface: &str, servers: &str) -> Option<Mechanism> {
    let list = split_servers(servers);
    if list.is_empty() {
        return None;
    }
    #[cfg(target_os = "linux")]
    return set_linux(interface, &list).await;
    #[cfg(target_os = "macos")]
    return set_macos(interface, &list).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (interface, list);
        None
    }
}

/// Restore the resolver configuration that was in effect before [`set`], scoped to
/// the tunnel interface, reversing the recorded mechanism.
pub async fn restore(interface: &str, mechanism: Mechanism) -> bool {
    #[cfg(target_os = "linux")]
    return restore_linux(interface, mechanism).await;
    #[cfg(target_os = "macos")]
    return restore_macos(interface, mechanism).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (interface, mechanism);
        true
    }
}

/// Split the comma-separated server list into validated IP address tokens, trimming
/// whitespace and dropping blank/malformed entries.
///
/// Each surviving token is spliced verbatim into the `resolvconf` stdin protocol
/// (one `nameserver <token>` line) or the `scutil` script (`d.add ServerAddresses *
/// <token>...`) - both line-oriented formats where an embedded space or newline
/// would inject an extra directive/command. Requiring a valid IP address rules
/// that out structurally instead of trying to blocklist bad characters.
fn split_servers(servers: &str) -> Vec<&str> {
    servers
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| match s.parse::<std::net::IpAddr>() {
            Ok(_) => true,
            Err(_) => {
                tracing::warn!(server = %s, "dropping non-IP DNS server entry");
                false
            }
        })
        .collect()
}

/// Argv for scoping the resolvers to the tunnel interface via systemd-resolved.
#[cfg(any(target_os = "linux", test))]
fn resolvectl_dns_args<'a>(interface: &'a str, servers: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["dns", interface];
    args.extend_from_slice(servers);
    args
}

/// Argv for routing all queries through the tunnel interface (`~.` is the
/// catch-all routing domain).
#[cfg(any(target_os = "linux", test))]
fn resolvectl_domain_args(interface: &str) -> [&str; 3] {
    ["domain", interface, "~."]
}

/// Argv for dropping the per-interface DNS + domain settings applied at setup.
#[cfg(any(target_os = "linux", test))]
fn resolvectl_revert_args(interface: &str) -> [&str; 2] {
    ["revert", interface]
}

/// Argv for registering the tunnel resolvers with resolvconf, matching wg-quick's
/// invocation; the servers are piped via stdin (see [`resolvconf_stdin`]).
#[cfg(any(target_os = "linux", test))]
fn resolvconf_add_args(interface: &str) -> [&str; 5] {
    ["-a", interface, "-m", "0", "-x"]
}

/// stdin payload for `resolvconf -a`: one `nameserver` line per server.
#[cfg(any(target_os = "linux", test))]
fn resolvconf_stdin(servers: &[&str]) -> String {
    servers.iter().map(|s| format!("nameserver {s}\n")).collect()
}

/// Argv for deregistering the tunnel resolvers from resolvconf.
#[cfg(any(target_os = "linux", test))]
fn resolvconf_del_args(interface: &str) -> [&str; 2] {
    ["-d", interface]
}

/// scutil script registering a supplemental resolver in the dynamic store keyed by
/// the tunnel interface (mirrors wg-quick).
#[cfg(any(target_os = "macos", test))]
fn scutil_set_script(interface: &str, servers: &[&str]) -> String {
    format!(
        "open\nd.init\nd.add ServerAddresses * {}\nset State:/Network/Service/{}/DNS\nquit\n",
        servers.join(" "),
        interface
    )
}

/// scutil script removing the supplemental resolver registered by
/// [`scutil_set_script`].
#[cfg(any(target_os = "macos", test))]
fn scutil_remove_script(interface: &str) -> String {
    format!("open\nremove State:/Network/Service/{interface}/DNS\nquit\n")
}

/// Run a command to completion, logging (never returning) failures. Reports whether
/// the command ran and exited successfully.
#[cfg(target_os = "linux")]
async fn run(what: &str, mut cmd: Command) -> bool {
    match cmd.status().await {
        Ok(status) if status.success() => true,
        Ok(status) => {
            tracing::warn!(%status, "{what} exited unsuccessfully (continuing)");
            false
        }
        Err(e) => {
            tracing::warn!(%e, "{what} failed to run (continuing)");
            false
        }
    }
}

#[cfg(target_os = "linux")]
async fn set_linux(interface: &str, servers: &[&str]) -> Option<Mechanism> {
    // systemd-resolved: scope the resolvers to the tunnel interface; `~.` routes every query through it.
    if resolved_owns_resolv_conf() {
        let mut dns = Command::new("resolvectl");
        dns.args(resolvectl_dns_args(interface, servers));
        if run("resolvectl dns", dns).await {
            let mut domain = Command::new("resolvectl");
            domain.args(resolvectl_domain_args(interface));
            run("resolvectl domain", domain).await;
            return Some(Mechanism::Resolvectl);
        }
    } else {
        tracing::info!("systemd-resolved does not own /etc/resolv.conf - skipping resolvectl");
    }
    // resolvconf for distros without systemd-resolved, mirroring wg-quick.
    if resolvconf_owns_resolv_conf() {
        if run_resolvconf_add(interface, servers).await {
            return Some(Mechanism::Resolvconf);
        }
    } else {
        tracing::info!("resolvconf does not own /etc/resolv.conf - skipping resolvconf");
    }
    // No resolver manager at all: edit the file ourselves, as Mullvad and Tailscale do.
    if resolv_conf::apply(servers) {
        tracing::info!(backup = resolv_conf::BACKUP, "managing /etc/resolv.conf directly");
        return Some(Mechanism::StaticFile);
    }
    tracing::warn!("no DNS mechanism took effect; DNS is not diverted through the tunnel (continuing)");
    None
}

/// resolved can run beside a NetworkManager-written file; then `resolvectl dns` succeeds while glibc never asks it.
#[cfg(target_os = "linux")]
fn resolved_owns_resolv_conf() -> bool {
    let target = std::fs::canonicalize(resolv_conf::RESOLV_CONF).ok();
    let content = std::fs::read_to_string(resolv_conf::RESOLV_CONF).unwrap_or_default();
    let nsswitch = std::fs::read_to_string("/etc/nsswitch.conf").unwrap_or_default();
    resolved_in_use(target.as_deref(), &content, &nsswitch)
}

#[cfg(target_os = "linux")]
fn resolved_in_use(resolv_conf_target: Option<&Path>, resolv_conf: &str, nsswitch: &str) -> bool {
    let is_resolved_file = resolv_conf_target.is_some_and(|p| p.starts_with("/run/systemd/resolve"));
    let uses_stub = resolv_conf::nameservers(resolv_conf)
        .iter()
        .any(|ns| ns == "127.0.0.53");
    let nss_resolve = nsswitch
        .lines()
        .filter(|line| line.starts_with("hosts:"))
        .any(|line| line.split_whitespace().any(|word| word == "resolve"));
    is_resolved_file || uses_stub || nss_resolve
}

/// Ubuntu's resolvconf is a resolvectl alias, and an installed resolvconf may not own the file; `-a` then changes nothing.
#[cfg(target_os = "linux")]
fn resolvconf_owns_resolv_conf() -> bool {
    let Some(binary) = find_in_path("resolvconf") else {
        return false;
    };
    let binary_target = std::fs::canonicalize(&binary).unwrap_or(binary);
    let file_target = std::fs::canonicalize(resolv_conf::RESOLV_CONF).ok();
    let content = std::fs::read_to_string(resolv_conf::RESOLV_CONF).unwrap_or_default();
    resolvconf_in_use(&binary_target, file_target.as_deref(), &content)
}

#[cfg(target_os = "linux")]
fn resolvconf_in_use(binary_target: &Path, resolv_conf_target: Option<&Path>, resolv_conf: &str) -> bool {
    let is_resolvectl_alias = binary_target.file_name() == Some(OsStr::new("resolvectl"));
    let links_into_run =
        resolv_conf_target.is_some_and(|p| p.starts_with("/run/resolvconf") || p.starts_with("/var/run/resolvconf"));
    // openresolv writes the file in place and only leaves its header behind.
    let generated_by_resolvconf = resolv_conf
        .lines()
        .any(|line| line.starts_with('#') && line.contains("resolvconf"));
    !is_resolvectl_alias && (links_into_run || generated_by_resolvconf)
}

#[cfg(target_os = "linux")]
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Sweep a backup left by a root that died while connected; unlike the state file it survives a reboot.
#[cfg(target_os = "linux")]
pub fn restore_leftover_resolv_conf() {
    if !resolv_conf::backup_exists() {
        return;
    }
    tracing::info!(
        backup = resolv_conf::BACKUP,
        "found a resolv.conf backup from an unclean exit - restoring it"
    );
    resolv_conf::restore_backup();
}

#[cfg(target_os = "linux")]
async fn run_resolvconf_add(interface: &str, servers: &[&str]) -> bool {
    use tokio::io::AsyncWriteExt;

    let mut child = match Command::new("resolvconf")
        .args(resolvconf_add_args(interface))
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(%e, "failed to spawn resolvconf for DNS (continuing)");
            return false;
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(resolvconf_stdin(servers).as_bytes()).await
    {
        tracing::warn!(%e, "failed to write resolvconf DNS payload (continuing)");
    }
    match child.wait().await {
        Ok(status) if status.success() => true,
        Ok(status) => {
            tracing::warn!(%status, "resolvconf -a exited unsuccessfully (continuing)");
            false
        }
        Err(e) => {
            tracing::warn!(%e, "resolvconf -a failed to run (continuing)");
            false
        }
    }
}

#[cfg(target_os = "linux")]
async fn restore_linux(interface: &str, mechanism: Mechanism) -> bool {
    match mechanism {
        Mechanism::Resolvectl => {
            // `revert` drops the per-interface DNS + domain settings applied above.
            let mut cmd = Command::new("resolvectl");
            cmd.args(resolvectl_revert_args(interface));
            run("resolvectl revert", cmd).await
        }
        Mechanism::Resolvconf => {
            let mut cmd = Command::new("resolvconf");
            cmd.args(resolvconf_del_args(interface));
            run("resolvconf -d", cmd).await
        }
        Mechanism::StaticFile => resolv_conf::restore(),
        Mechanism::Scutil => {
            tracing::warn!("recorded DNS mechanism scutil does not apply on Linux (skipping restore)");
            false
        }
    }
}

#[cfg(target_os = "macos")]
async fn set_macos(interface: &str, servers: &[&str]) -> Option<Mechanism> {
    // Mirror wg-quick: register a supplemental resolver in the dynamic store keyed
    // by the tunnel interface. Removed on restore.
    if run_scutil(&scutil_set_script(interface, servers)).await {
        Some(Mechanism::Scutil)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
async fn restore_macos(interface: &str, mechanism: Mechanism) -> bool {
    match mechanism {
        Mechanism::Scutil => run_scutil(&scutil_remove_script(interface)).await,
        Mechanism::Resolvectl | Mechanism::Resolvconf | Mechanism::StaticFile => {
            tracing::warn!(
                ?mechanism,
                "recorded DNS mechanism does not apply on macOS (skipping restore)"
            );
            false
        }
    }
}

/// Run a scutil script to completion, logging (never returning) failures. Reports
/// whether the command ran and exited successfully.
#[cfg(target_os = "macos")]
async fn run_scutil(script: &str) -> bool {
    use tokio::io::AsyncWriteExt;

    let mut child = match Command::new("scutil").stdin(std::process::Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(%e, "failed to spawn scutil for DNS (continuing)");
            return false;
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(script.as_bytes()).await
    {
        tracing::warn!(%e, "failed to write scutil DNS script (continuing)");
    }
    match child.wait().await {
        Ok(status) if status.success() => true,
        Ok(status) => {
            tracing::warn!(%status, "scutil DNS command exited unsuccessfully (continuing)");
            false
        }
        Err(e) => {
            tracing::warn!(%e, "scutil DNS command failed (continuing)");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_comma_separated_servers() {
        assert_eq!(split_servers("1.1.1.1,8.8.8.8"), vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn split_trims_whitespace_and_drops_blank_entries() {
        assert_eq!(split_servers(" 1.1.1.1 , ,8.8.8.8, "), vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn split_of_blank_list_is_empty() {
        assert!(split_servers("").is_empty());
        assert!(split_servers(" , ").is_empty());
    }

    #[test]
    fn split_drops_entries_that_are_not_a_bare_ip_address() {
        // A hostname, and a token smuggling a second resolvconf/scutil directive
        // via embedded whitespace, must both be dropped rather than passed through.
        assert_eq!(split_servers("1.1.1.1,example.com,8.8.8.8"), vec!["1.1.1.1", "8.8.8.8"]);
        assert_eq!(split_servers("1.1.1.1,8.8.8.8 domain evil.example"), vec!["1.1.1.1"]);
    }

    #[test]
    fn resolvectl_args_scope_servers_and_catchall_domain_to_interface() {
        assert_eq!(
            resolvectl_dns_args("wg0_gnosisvpn", &["1.1.1.1", "8.8.8.8"]),
            vec!["dns", "wg0_gnosisvpn", "1.1.1.1", "8.8.8.8"]
        );
        assert_eq!(
            resolvectl_domain_args("wg0_gnosisvpn"),
            ["domain", "wg0_gnosisvpn", "~."]
        );
        assert_eq!(resolvectl_revert_args("wg0_gnosisvpn"), ["revert", "wg0_gnosisvpn"]);
    }

    #[test]
    fn resolvconf_commands_match_wg_quick_verbatim() {
        assert_eq!(
            format!("resolvconf {}", resolvconf_add_args("wg0_gnosisvpn").join(" ")),
            "resolvconf -a wg0_gnosisvpn -m 0 -x"
        );
        assert_eq!(
            format!("resolvconf {}", resolvconf_del_args("wg0_gnosisvpn").join(" ")),
            "resolvconf -d wg0_gnosisvpn"
        );
    }

    #[test]
    fn resolvconf_stdin_is_one_nameserver_line_per_server() {
        assert_eq!(
            resolvconf_stdin(&["1.1.1.1", "8.8.8.8"]),
            "nameserver 1.1.1.1\nnameserver 8.8.8.8\n"
        );
    }

    #[test]
    fn scutil_scripts_target_the_interface_service_key() {
        assert_eq!(
            scutil_set_script("utun8", &["1.1.1.1", "8.8.8.8"]),
            "open\nd.init\nd.add ServerAddresses * 1.1.1.1 8.8.8.8\nset State:/Network/Service/utun8/DNS\nquit\n"
        );
        assert_eq!(
            scutil_remove_script("utun8"),
            "open\nremove State:/Network/Service/utun8/DNS\nquit\n"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    fn resolved_is_detected_by_stub_file_or_nss_module() {
        let stub = Some(Path::new("/run/systemd/resolve/stub-resolv.conf"));
        assert!(resolved_in_use(stub, "nameserver 127.0.0.53\n", ""));
        assert!(resolved_in_use(
            Some(Path::new("/etc/resolv.conf")),
            "nameserver 127.0.0.53\n",
            ""
        ));
        assert!(resolved_in_use(
            Some(Path::new("/etc/resolv.conf")),
            "nameserver 192.168.1.1\n",
            "hosts: files resolve [!UNAVAIL=return] dns\n"
        ));
    }

    #[test]
    fn resolved_running_next_to_a_networkmanager_file_does_not_count() {
        let plain = Some(Path::new("/etc/resolv.conf"));
        let nm_file = "# Generated by NetworkManager\nnameserver 192.168.122.1\n";
        assert!(!resolved_in_use(
            plain,
            nm_file,
            "hosts: files mdns4_minimal [NOTFOUND=return] dns\n"
        ));
        assert!(!resolved_in_use(None, "", ""));
    }

    #[test]
    fn resolvconf_counts_only_when_it_owns_the_file() {
        let debian = Path::new("/usr/sbin/resolvconf");
        let run_file = Some(Path::new("/run/resolvconf/resolv.conf"));
        let plain = Some(Path::new("/etc/resolv.conf"));
        assert!(resolvconf_in_use(debian, run_file, ""));
        assert!(resolvconf_in_use(
            debian,
            plain,
            "# Generated by resolvconf\nnameserver 10.0.0.1\n"
        ));
        // Installed while another program still owns the file.
        assert!(!resolvconf_in_use(
            debian,
            plain,
            "# Generated by NetworkManager\nnameserver 10.0.0.1\n"
        ));
    }

    #[test]
    fn ubuntu_resolvectl_alias_is_not_resolvconf() {
        let alias = Path::new("/usr/bin/resolvectl");
        assert!(!resolvconf_in_use(
            alias,
            Some(Path::new("/run/resolvconf/resolv.conf")),
            ""
        ));
    }
}
