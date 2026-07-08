//! Crash-recovery sweep for tunnel side effects that survive an unclean root exit.
//!
//! Routes bound to the TUN interface vanish with root's fd when the process dies,
//! but several side effects do not: the IPv6 blackhole routes, the DNS diversion
//! (resolvectl/resolvconf on Linux, the `State:/Network/Service/<utunN>/DNS` scutil
//! key on macOS), the WAN-scoped bypass routes (peer `/32` + RFC1918, pinned to the
//! physical device), and the killswitch firewall lockdown. Tunnel setup records the
//! interface, DNS mechanism, blackhole status, and bypass routes in a small state
//! file which clean teardown deletes; if the file is still present at the next
//! daemon start, the recorded side effects are removed best-effort. The killswitch
//! is reset unconditionally by name at every start, since its default-drop lockdown
//! would otherwise leave the host with no connectivity after a crash.

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use super::{dns, ipv6_blackhole};

/// Side effects recorded at tunnel setup so a SIGKILLed root can be swept at the
/// next daemon start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeardownState {
    /// Resolved TUN interface name (`wg0_gnosisvpn` on Linux, `utunN` on macOS).
    pub interface_name: String,
    /// DNS mechanism that took effect at setup, if any.
    pub dns_mechanism_applied: Option<dns::Mechanism>,
    /// Whether the IPv6 blackhole routes were added.
    pub blackholes_added: bool,
    /// WAN-scoped bypass routes installed at setup and as peers are discovered:
    /// `(dest_cidr, wan_device)`. Pinned to the physical device, so unlike TUN
    /// routes they survive an unclean exit and must be removed explicitly. Defaults
    /// to empty when reading a state file written before this field existed.
    #[serde(default)]
    pub bypass_routes: Vec<(String, String)>,
}

/// Preferred and fallback locations for the state file. Root can normally write
/// under `/var/run`; `/tmp` covers environments where it cannot.
fn candidate_paths() -> [PathBuf; 2] {
    [
        PathBuf::from("/var/run/gnosisvpn/teardown-state.json"),
        PathBuf::from("/tmp/gnosisvpn-teardown-state.json"),
    ]
}

/// Persist `state` so a crashed root can be swept at the next start. Best-effort:
/// losing the record only costs crash recovery, never the connection.
pub fn record(state: &TeardownState) {
    record_at(&candidate_paths(), state);
}

/// Delete the persisted record after a clean teardown.
pub fn clear() {
    clear_at(&candidate_paths());
}

/// Sweep tunnel side effects recorded by a previous root that exited without
/// tearing down (e.g. SIGKILL). Route table entries bound to the TUN died with its
/// fd; only the IPv6 blackhole routes and the DNS diversion survive and are
/// removed here, best-effort.
pub async fn startup_sweep() {
    // The killswitch default-drop firewall survives an unclean exit independently of
    // the state file (and can outlive a lost state file), so reset it unconditionally
    // by name first - otherwise a crashed root leaves the host firewalled off with no
    // connectivity until the next successful connect.
    reset_leftover_killswitch();

    let Some(state) = take_leftover_at(&candidate_paths()) else {
        return;
    };
    tracing::info!(
        interface = %state.interface_name,
        dns_mechanism = ?state.dns_mechanism_applied,
        blackholes = state.blackholes_added,
        bypass_routes = state.bypass_routes.len(),
        "found teardown state from an unclean exit - sweeping leftover tunnel side effects"
    );
    if state.blackholes_added {
        ipv6_blackhole::remove().await;
    }
    if let Some(mechanism) = state.dns_mechanism_applied {
        dns::restore(&state.interface_name, mechanism).await;
    }
    remove_bypass_routes(&state.bypass_routes).await;
    tracing::info!("crash-recovery sweep complete");
}

/// Reset the killswitch firewall by its fixed anchor/table name. Idempotent and safe
/// to call when nothing is installed, so it runs unconditionally at every start.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reset_leftover_killswitch() {
    match gnosis_vpn_lib::killswitch::Firewall::new() {
        Ok(mut firewall) => {
            if let Err(e) = firewall.reset_policy() {
                tracing::warn!(%e, "failed to reset leftover killswitch at startup (continuing)");
            }
        }
        Err(e) => tracing::warn!(%e, "failed to build firewall for startup killswitch reset (continuing)"),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reset_leftover_killswitch() {}

/// Remove WAN-scoped bypass routes recorded by a previous root. Best-effort: a route
/// that is already gone (or whose WAN device changed) just logs a warning.
#[cfg(target_os = "macos")]
async fn remove_bypass_routes(routes: &[(String, String)]) {
    use super::route_ops::RouteOps;
    let ops = super::route_ops_macos::DarwinRouteOps;
    for (dest, device) in routes {
        if let Err(e) = ops.route_del(dest, device).await {
            tracing::warn!(%e, dest = %dest, "failed to remove leftover bypass route (continuing)");
        }
    }
}

#[cfg(target_os = "linux")]
async fn remove_bypass_routes(routes: &[(String, String)]) {
    use super::route_ops::RouteOps;
    if routes.is_empty() {
        return;
    }
    let (connection, handle, _) = match rtnetlink::new_connection() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(%e, "failed to open netlink for bypass-route sweep (continuing)");
            return;
        }
    };
    tokio::spawn(connection);
    let ops = super::route_ops_linux::NetlinkRouteOps::new(handle);
    for (dest, device) in routes {
        if let Err(e) = ops.route_del(dest, device).await {
            tracing::warn!(%e, dest = %dest, device = %device, "failed to remove leftover bypass route (continuing)");
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn remove_bypass_routes(_routes: &[(String, String)]) {}

/// Persist `state` at the first writable candidate path.
fn record_at(paths: &[PathBuf], state: &TeardownState) {
    let payload = match serde_json::to_string(state) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::warn!(%e, "failed to serialize teardown state (crash recovery disabled for this tunnel)");
            return;
        }
    };
    for path in paths {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(path, &payload) {
            Ok(()) => {
                tracing::debug!(path = %path.display(), "recorded teardown state");
                return;
            }
            Err(e) => {
                tracing::debug!(%e, path = %path.display(), "could not write teardown state here (trying next location)")
            }
        }
    }
    tracing::warn!("failed to persist teardown state (crash recovery disabled for this tunnel)");
}

/// Remove the state file from every candidate path.
fn clear_at(paths: &[PathBuf]) {
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => tracing::debug!(path = %path.display(), "cleared teardown state"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(%e, path = %path.display(), "failed to remove teardown state file"),
        }
    }
}

/// Load and consume a leftover state file, if any.
fn take_leftover_at(paths: &[PathBuf]) -> Option<TeardownState> {
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        // Delete before parsing so a corrupt file cannot wedge every future start.
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(%e, path = %path.display(), "failed to remove teardown state file");
        }
        match serde_json::from_slice(&bytes) {
            Ok(state) => return Some(state),
            Err(e) => tracing::warn!(%e, path = %path.display(), "ignoring unparseable teardown state file"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> TeardownState {
        TeardownState {
            interface_name: "utun8".to_string(),
            dns_mechanism_applied: Some(dns::Mechanism::Scutil),
            blackholes_added: true,
            bypass_routes: vec![("35.213.7.172".to_string(), "en0".to_string())],
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gvpn-sweep-{name}-{}", std::process::id()))
    }

    #[test]
    fn state_survives_a_serde_round_trip() -> anyhow::Result<()> {
        let state = sample_state();
        let json = serde_json::to_string(&state)?;
        let parsed: TeardownState = serde_json::from_str(&json)?;
        assert_eq!(parsed, state);
        Ok(())
    }

    #[test]
    fn state_without_dns_survives_a_serde_round_trip() -> anyhow::Result<()> {
        let state = TeardownState {
            interface_name: "wg0_gnosisvpn".to_string(),
            dns_mechanism_applied: None,
            blackholes_added: false,
            bypass_routes: Vec::new(),
        };
        let json = serde_json::to_string(&state)?;
        let parsed: TeardownState = serde_json::from_str(&json)?;
        assert_eq!(parsed, state);
        Ok(())
    }

    #[test]
    fn legacy_state_without_bypass_routes_field_defaults_to_empty() -> anyhow::Result<()> {
        // A state file written before the bypass_routes field existed must still
        // parse (upgrade path), with bypass_routes defaulting to empty.
        let mut value = serde_json::to_value(sample_state())?;
        value
            .as_object_mut()
            .expect("state serializes to an object")
            .remove("bypass_routes");
        let parsed: TeardownState = serde_json::from_value(value)?;
        assert!(parsed.bypass_routes.is_empty());
        Ok(())
    }

    #[test]
    fn bypass_routes_survive_a_serde_round_trip() -> anyhow::Result<()> {
        let state = sample_state();
        let parsed: TeardownState = serde_json::from_str(&serde_json::to_string(&state)?)?;
        assert_eq!(parsed.bypass_routes, state.bypass_routes);
        Ok(())
    }

    #[test]
    fn prefers_var_run_with_tmp_fallback() {
        let paths = candidate_paths();
        assert_eq!(paths[0], PathBuf::from("/var/run/gnosisvpn/teardown-state.json"));
        assert_eq!(paths[1], PathBuf::from("/tmp/gnosisvpn-teardown-state.json"));
    }

    #[test]
    fn state_file_round_trips_through_record_and_take() {
        let dir = test_dir("roundtrip");
        let paths = [dir.join("teardown-state.json")];
        let state = sample_state();
        record_at(&paths, &state);
        assert_eq!(take_leftover_at(&paths), Some(state));
        // consumed on take, so a second sweep finds nothing
        assert_eq!(take_leftover_at(&paths), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_falls_back_when_the_first_location_is_unwritable() -> anyhow::Result<()> {
        let dir = test_dir("fallback");
        std::fs::create_dir_all(&dir)?;
        // A plain file where the first candidate expects its parent directory
        // makes that location unwritable.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"")?;
        let paths = [blocker.join("teardown-state.json"), dir.join("teardown-state.json")];
        let state = sample_state();
        record_at(&paths, &state);
        assert_eq!(take_leftover_at(&paths), Some(state));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn unparseable_state_file_is_consumed_and_ignored() -> anyhow::Result<()> {
        let dir = test_dir("unparseable");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("teardown-state.json");
        std::fs::write(&path, "not json")?;
        let paths = [path.clone()];
        assert_eq!(take_leftover_at(&paths), None);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn clear_removes_the_state_file() {
        let dir = test_dir("clear");
        let paths = [dir.join("teardown-state.json")];
        record_at(&paths, &sample_state());
        clear_at(&paths);
        assert_eq!(take_leftover_at(&paths), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
