//! DNS management for the NepTUN data plane, taking over what `wg-quick` used to
//! do internally.
//!
//! Best-effort by design: a failure to set or restore DNS logs a warning and never
//! fails the connection - traffic still routes, only DNS-leak prevention is
//! affected. The exact resolver plumbing (systemd-resolved vs. resolvconf on Linux,
//! `scutil` supplemental resolvers on macOS) is environment-specific and must be
//! validated end-to-end with root on both platforms.

use tokio::process::Command;

/// Push `servers` (a comma-separated list) as the DNS resolvers scoped to the
/// tunnel interface. An empty/blank list is a no-op.
pub async fn set(interface: &str, servers: &str) {
    let list: Vec<&str> = servers.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if list.is_empty() {
        return;
    }
    #[cfg(target_os = "linux")]
    set_linux(interface, &list).await;
    #[cfg(target_os = "macos")]
    set_macos(interface, &list).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (interface, list);
}

/// Restore the resolver configuration that was in effect before [`set`], scoped to
/// the tunnel interface.
pub async fn restore(interface: &str) {
    #[cfg(target_os = "linux")]
    restore_linux(interface).await;
    #[cfg(target_os = "macos")]
    restore_macos(interface).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = interface;
}

/// Run a command to completion, logging (never returning) failures.
#[cfg(target_os = "linux")]
async fn run(what: &str, mut cmd: Command) {
    match cmd.status().await {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(%status, "{what} exited unsuccessfully (continuing)"),
        Err(e) => tracing::warn!(%e, "{what} failed to run (continuing)"),
    }
}

#[cfg(target_os = "linux")]
async fn set_linux(interface: &str, servers: &[&str]) {
    // systemd-resolved: scope the resolvers to the tunnel interface and route all
    // queries through it (`~.` is the catch-all routing domain).
    let mut dns = Command::new("resolvectl");
    dns.arg("dns").arg(interface).args(servers);
    run("resolvectl dns", dns).await;
    let mut domain = Command::new("resolvectl");
    domain.arg("domain").arg(interface).arg("~.");
    run("resolvectl domain", domain).await;
}

#[cfg(target_os = "linux")]
async fn restore_linux(interface: &str) {
    // `revert` drops the per-interface DNS + domain settings applied above.
    let mut cmd = Command::new("resolvectl");
    cmd.arg("revert").arg(interface);
    run("resolvectl revert", cmd).await;
}

#[cfg(target_os = "macos")]
async fn set_macos(interface: &str, servers: &[&str]) {
    // Mirror wg-quick: register a supplemental resolver in the dynamic store keyed
    // by the tunnel interface. Removed on restore.
    let script = format!(
        "open\nd.init\nd.add ServerAddresses * {}\nset State:/Network/Service/{}/DNS\nquit\n",
        servers.join(" "),
        interface
    );
    run_scutil(&script).await;
}

#[cfg(target_os = "macos")]
async fn restore_macos(interface: &str) {
    let script = format!("open\nremove State:/Network/Service/{interface}/DNS\nquit\n");
    run_scutil(&script).await;
}

#[cfg(target_os = "macos")]
async fn run_scutil(script: &str) {
    use tokio::io::AsyncWriteExt;

    let mut child = match Command::new("scutil").stdin(std::process::Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(%e, "failed to spawn scutil for DNS (continuing)");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(script.as_bytes()).await
    {
        tracing::warn!(%e, "failed to write scutil DNS script (continuing)");
    }
    match child.wait().await {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(%status, "scutil DNS command exited unsuccessfully (continuing)"),
        Err(e) => tracing::warn!(%e, "scutil DNS command failed (continuing)"),
    }
}
