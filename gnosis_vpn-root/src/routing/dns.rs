//! DNS management for the NepTUN data plane, taking over what `wg-quick` used to
//! do internally.
//!
//! Best-effort by design: a failure to set or restore DNS logs a warning and never
//! fails the connection - traffic still routes, only DNS-leak prevention is
//! affected. The exact resolver plumbing (systemd-resolved vs. resolvconf on Linux,
//! `scutil` supplemental resolvers on macOS) is environment-specific and must be
//! validated end-to-end with root on both platforms.

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// The resolver mechanism used to apply DNS at setup, recorded so teardown (and the
/// crash-recovery sweep after an unclean exit) reverses the matching one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mechanism {
    /// systemd-resolved via `resolvectl` (Linux).
    Resolvectl,
    /// resolvconf via `resolvconf -a`/`-d` (Linux distros without systemd-resolved).
    Resolvconf,
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
pub async fn restore(interface: &str, mechanism: Mechanism) {
    #[cfg(target_os = "linux")]
    restore_linux(interface, mechanism).await;
    #[cfg(target_os = "macos")]
    restore_macos(interface, mechanism).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (interface, mechanism);
}

/// Split the comma-separated server list, trimming whitespace and dropping blanks.
fn split_servers(servers: &str) -> Vec<&str> {
    servers.split(',').map(str::trim).filter(|s| !s.is_empty()).collect()
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
    // systemd-resolved: scope the resolvers to the tunnel interface and route all
    // queries through it (`~.` is the catch-all routing domain).
    let mut dns = Command::new("resolvectl");
    dns.args(resolvectl_dns_args(interface, servers));
    if run("resolvectl dns", dns).await {
        let mut domain = Command::new("resolvectl");
        domain.args(resolvectl_domain_args(interface));
        run("resolvectl domain", domain).await;
        return Some(Mechanism::Resolvectl);
    }
    // resolvconf fallback for distros without systemd-resolved, mirroring wg-quick.
    tracing::info!("resolvectl unavailable - falling back to resolvconf");
    if run_resolvconf_add(interface, servers).await {
        return Some(Mechanism::Resolvconf);
    }
    tracing::warn!("no DNS mechanism took effect; DNS is not diverted through the tunnel (continuing)");
    None
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
async fn restore_linux(interface: &str, mechanism: Mechanism) {
    match mechanism {
        Mechanism::Resolvectl => {
            // `revert` drops the per-interface DNS + domain settings applied above.
            let mut cmd = Command::new("resolvectl");
            cmd.args(resolvectl_revert_args(interface));
            run("resolvectl revert", cmd).await;
        }
        Mechanism::Resolvconf => {
            let mut cmd = Command::new("resolvconf");
            cmd.args(resolvconf_del_args(interface));
            run("resolvconf -d", cmd).await;
        }
        Mechanism::Scutil => {
            tracing::warn!("recorded DNS mechanism scutil does not apply on Linux (skipping restore)");
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
async fn restore_macos(interface: &str, mechanism: Mechanism) {
    match mechanism {
        Mechanism::Scutil => {
            run_scutil(&scutil_remove_script(interface)).await;
        }
        Mechanism::Resolvectl | Mechanism::Resolvconf => {
            tracing::warn!(
                ?mechanism,
                "recorded DNS mechanism does not apply on macOS (skipping restore)"
            );
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
