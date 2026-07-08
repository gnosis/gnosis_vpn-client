//! IPv6 blackhole routes preventing v6 leakage while the tunnel is IPv4-only
//! (previously emitted as wg-quick PreUp/PostDown config lines).
//!
//! Two /1 halves cover all of IPv6 space and are more specific than any
//! router-provided default. Best-effort by design: a failure logs a warning and
//! never fails the connection.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::process::Command;

/// The two /1 halves that together blackhole all of IPv6 space.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const BLACKHOLE_DESTINATIONS: [&str; 2] = ["::/1", "8000::/1"];

/// Argv for `ip` to add one blackhole destination on Linux.
#[cfg(any(target_os = "linux", test))]
fn linux_add_args(dst: &str) -> [&str; 5] {
    ["-6", "route", "add", "blackhole", dst]
}

/// Argv for `ip` to delete one blackhole destination on Linux.
#[cfg(any(target_os = "linux", test))]
fn linux_del_args(dst: &str) -> [&str; 5] {
    ["-6", "route", "del", "blackhole", dst]
}

/// Argv for `route` to add one blackhole destination on macOS.
#[cfg(any(target_os = "macos", test))]
fn macos_add_args(dst: &str) -> [&str; 6] {
    ["-n", "add", "-blackhole", "-inet6", dst, "::1"]
}

/// Argv for `route` to delete one blackhole destination on macOS.
#[cfg(any(target_os = "macos", test))]
fn macos_delete_args(dst: &str) -> [&str; 6] {
    ["-n", "delete", "-blackhole", "-inet6", dst, "::1"]
}

/// Add the blackhole routes. Idempotent (del-before-add) so a stale route never
/// blocks setup. Best-effort.
#[cfg(target_os = "linux")]
pub async fn add() {
    for dst in BLACKHOLE_DESTINATIONS {
        let _ = run_ip6(&linux_del_args(dst)).await;
        if let Err(e) = run_ip6(&linux_add_args(dst)).await {
            tracing::warn!(%e, dst, "failed to add IPv6 blackhole route (continuing)");
        }
    }
}

/// Remove the blackhole routes. Best-effort.
#[cfg(target_os = "linux")]
pub async fn remove() {
    for dst in BLACKHOLE_DESTINATIONS {
        if let Err(e) = run_ip6(&linux_del_args(dst)).await {
            tracing::warn!(%e, dst, "failed to remove IPv6 blackhole route (continuing)");
        }
    }
}

#[cfg(target_os = "linux")]
async fn run_ip6(args: &[&str]) -> Result<(), gnosis_vpn_lib::shell_command_ext::Error> {
    Command::new("ip").args(args).run(Logs::Suppress).await
}

/// Add the blackhole routes. Idempotent (del-before-add) so a stale route left by an
/// unclean exit never blocks setup. Best-effort.
#[cfg(target_os = "macos")]
pub async fn add() {
    for dst in BLACKHOLE_DESTINATIONS {
        let mut del = Command::new("route");
        del.args(macos_delete_args(dst));
        let _ = del.run(Logs::Suppress).await;
        let mut cmd = Command::new("route");
        cmd.args(macos_add_args(dst));
        if let Err(e) = cmd.run(Logs::Suppress).await {
            tracing::warn!(%e, dst, "failed to add IPv6 blackhole route (continuing)");
        }
    }
}

/// Remove the blackhole routes. Best-effort.
#[cfg(target_os = "macos")]
pub async fn remove() {
    for dst in BLACKHOLE_DESTINATIONS {
        let mut cmd = Command::new("route");
        cmd.args(macos_delete_args(dst));
        if let Err(e) = cmd.run(Logs::Suppress).await {
            tracing::warn!(%e, dst, "failed to remove IPv6 blackhole route (continuing)");
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub async fn add() {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub async fn remove() {}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim wg-quick commands documented in
    // docs/neptun-phase3-4-testing-guide.md section 3e.

    #[test]
    fn linux_argv_matches_wg_quick_verbatim() {
        let add: Vec<String> = BLACKHOLE_DESTINATIONS
            .iter()
            .map(|dst| format!("ip {}", linux_add_args(dst).join(" ")))
            .collect();
        assert_eq!(
            add,
            ["ip -6 route add blackhole ::/1", "ip -6 route add blackhole 8000::/1"]
        );
        let del: Vec<String> = BLACKHOLE_DESTINATIONS
            .iter()
            .map(|dst| format!("ip {}", linux_del_args(dst).join(" ")))
            .collect();
        assert_eq!(
            del,
            ["ip -6 route del blackhole ::/1", "ip -6 route del blackhole 8000::/1"]
        );
    }

    #[test]
    fn macos_argv_matches_wg_quick_verbatim() {
        let add: Vec<String> = BLACKHOLE_DESTINATIONS
            .iter()
            .map(|dst| format!("route {}", macos_add_args(dst).join(" ")))
            .collect();
        assert_eq!(
            add,
            [
                "route -n add -blackhole -inet6 ::/1 ::1",
                "route -n add -blackhole -inet6 8000::/1 ::1"
            ]
        );
        let del: Vec<String> = BLACKHOLE_DESTINATIONS
            .iter()
            .map(|dst| format!("route {}", macos_delete_args(dst).join(" ")))
            .collect();
        assert_eq!(
            del,
            [
                "route -n delete -blackhole -inet6 ::/1 ::1",
                "route -n delete -blackhole -inet6 8000::/1 ::1"
            ]
        );
    }
}
