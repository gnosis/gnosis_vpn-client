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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeardownState {
    /// Resolved TUN interface name (`wg0_gnosisvpn` on Linux, `utunN` on macOS).
    #[serde(default)]
    pub interface_name: Option<String>,
    /// DNS mechanism that took effect at setup, if any.
    #[serde(default)]
    pub dns_mechanism_applied: Option<dns::Mechanism>,
    /// Whether the IPv6 blackhole routes were added.
    #[serde(default)]
    pub blackholes_added: bool,
    /// WAN-scoped bypass routes installed at setup and as peers are discovered:
    /// `(dest_cidr, wan_device)`. Pinned to the physical device, so unlike TUN
    /// routes they survive an unclean exit and must be removed explicitly. Defaults
    /// to empty when reading a state file written before this field existed.
    #[serde(default)]
    pub bypass_routes: Vec<(String, String)>,
    /// Whether the named killswitch policy needs to be reset.
    #[serde(default)]
    pub killswitch_active: bool,
    /// Whether PF was enabled before the macOS killswitch first changed it.
    #[serde(default)]
    pub pf_was_enabled: Option<bool>,
}

impl TeardownState {
    pub fn is_empty(&self) -> bool {
        self.dns_mechanism_applied.is_none()
            && !self.blackholes_added
            && self.bypass_routes.is_empty()
            && !self.killswitch_active
    }
}

/// Root-owned location for the teardown state file.
fn candidate_paths() -> [PathBuf; 1] {
    [PathBuf::from("/var/run/gnosisvpn/teardown-state.json")]
}

/// Persist `state` so a crashed root can be swept at the next start. Best-effort:
/// losing the record only costs crash recovery, never the connection.
pub fn record(state: &TeardownState) {
    record_routing_at(&candidate_paths(), state);
}

/// Delete the persisted record after a clean teardown.
#[allow(dead_code)]
pub fn clear() {
    clear_routing_at(&candidate_paths());
}

/// Persist killswitch recovery without overwriting routing recovery owned by the
/// static router.
pub fn record_killswitch(pf_was_enabled: Option<bool>) {
    record_killswitch_at(&candidate_paths(), pf_was_enabled);
}

/// Clear only killswitch recovery after a successful reset.
pub fn clear_killswitch() {
    clear_killswitch_at(&candidate_paths());
}

/// Sweep tunnel side effects recorded by a previous root that exited without
/// tearing down (e.g. SIGKILL). Route table entries bound to the TUN died with its
/// fd; only the IPv6 blackhole routes and the DNS diversion survive and are
/// removed here, best-effort.
pub async fn startup_sweep() {
    let paths = candidate_paths();
    let mut state = load_leftover_at(&paths).unwrap_or_default();
    // The killswitch default-drop firewall survives an unclean exit independently of
    // the state file (and can outlive a lost state file), so reset it unconditionally
    // by name first - otherwise a crashed root leaves the host firewalled off with no
    // connectivity until the next successful connect.
    if reset_leftover_killswitch(state.pf_was_enabled) {
        state.killswitch_active = false;
        state.pf_was_enabled = None;
    } else {
        state.killswitch_active = true;
    }

    if state.is_empty() {
        clear_at(&paths);
        return;
    }
    tracing::info!(
        interface = ?state.interface_name,
        dns_mechanism = ?state.dns_mechanism_applied,
        blackholes = state.blackholes_added,
        bypass_routes = state.bypass_routes.len(),
        "found teardown state from an unclean exit - sweeping leftover tunnel side effects"
    );
    if state.blackholes_added && ipv6_blackhole::remove().await {
        state.blackholes_added = false;
    }
    if let (Some(mechanism), Some(interface_name)) = (state.dns_mechanism_applied, state.interface_name.as_deref()) {
        if dns::restore(interface_name, mechanism).await {
            state.dns_mechanism_applied = None;
        }
    } else if state.dns_mechanism_applied.is_some() {
        tracing::warn!("cannot restore leftover DNS because its interface name was not recorded");
    }
    state.bypass_routes = remove_bypass_routes(&state.bypass_routes).await;
    if state.is_empty() {
        clear_at(&paths);
    } else {
        record_at(&paths, &state);
    }
    tracing::info!("crash-recovery sweep complete");
}

/// Reset the killswitch firewall by its fixed anchor/table name. Idempotent and safe
/// to call when nothing is installed, so it runs unconditionally at every start.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reset_leftover_killswitch(pf_was_enabled: Option<bool>) -> bool {
    match gnosis_vpn_lib::killswitch::Firewall::new() {
        Ok(mut firewall) => {
            if let Err(e) = firewall.reset_policy_with_state(pf_was_enabled) {
                tracing::warn!(%e, "failed to reset leftover killswitch at startup (continuing)");
                false
            } else {
                true
            }
        }
        Err(e) => {
            tracing::warn!(%e, "failed to build firewall for startup killswitch reset (continuing)");
            false
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reset_leftover_killswitch(_pf_was_enabled: Option<bool>) -> bool {
    true
}

/// Remove WAN-scoped bypass routes recorded by a previous root. Best-effort: a route
/// that is already gone (or whose WAN device changed) just logs a warning.
#[cfg(target_os = "macos")]
async fn remove_bypass_routes(routes: &[(String, String)]) -> Vec<(String, String)> {
    use super::route_ops::RouteOps;
    let ops = super::route_ops_macos::DarwinRouteOps;
    let mut failed = Vec::new();
    for (dest, device) in routes {
        if let Err(e) = ops.route_del(dest, device).await {
            tracing::warn!(%e, dest = %dest, "failed to remove leftover bypass route (continuing)");
            failed.push((dest.clone(), device.clone()));
        }
    }
    failed
}

#[cfg(target_os = "linux")]
async fn remove_bypass_routes(routes: &[(String, String)]) -> Vec<(String, String)> {
    use super::route_ops::RouteOps;
    if routes.is_empty() {
        return Vec::new();
    }
    let (connection, handle, _) = match rtnetlink::new_connection() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(%e, "failed to open netlink for bypass-route sweep (continuing)");
            return routes.to_vec();
        }
    };
    tokio::spawn(connection);
    let ops = super::route_ops_linux::NetlinkRouteOps::new(handle);
    let mut failed = Vec::new();
    for (dest, device) in routes {
        if let Err(e) = ops.route_del(dest, device).await {
            tracing::warn!(%e, dest = %dest, device = %device, "failed to remove leftover bypass route (continuing)");
            failed.push((dest.clone(), device.clone()));
        }
    }
    failed
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn remove_bypass_routes(_routes: &[(String, String)]) -> Vec<(String, String)> {
    Vec::new()
}

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
        match write_owner_only(path, payload.as_bytes()) {
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

fn mutate_at(paths: &[PathBuf], update: impl FnOnce(&mut TeardownState)) {
    let mut state = load_leftover_at(paths).unwrap_or_default();
    update(&mut state);
    if state.is_empty() {
        clear_at(paths);
    } else {
        record_at(paths, &state);
    }
}

fn record_routing_at(paths: &[PathBuf], routing: &TeardownState) {
    mutate_at(paths, |state| {
        state.interface_name.clone_from(&routing.interface_name);
        state.dns_mechanism_applied = routing.dns_mechanism_applied;
        state.blackholes_added = routing.blackholes_added;
        state.bypass_routes.clone_from(&routing.bypass_routes);
    });
}

fn clear_routing_at(paths: &[PathBuf]) {
    mutate_at(paths, |state| {
        state.interface_name = None;
        state.dns_mechanism_applied = None;
        state.blackholes_added = false;
        state.bypass_routes.clear();
    });
}

fn record_killswitch_at(paths: &[PathBuf], pf_was_enabled: Option<bool>) {
    mutate_at(paths, |state| {
        state.killswitch_active = true;
        state.pf_was_enabled = pf_was_enabled;
    });
}

fn clear_killswitch_at(paths: &[PathBuf]) {
    mutate_at(paths, |state| {
        state.killswitch_active = false;
        state.pf_was_enabled = None;
    });
}

/// Write `payload` at `path` readable by the owner only (0600): the file holds
/// routing/DNS details of a privileged tunnel. The mode is enforced via `fchmod`
/// on the open handle, so a pre-existing file with looser permissions is
/// tightened as well.
fn write_owner_only(path: &std::path::Path, payload: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("teardown-state");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(payload)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
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

/// Load a leftover state file without consuming valid recovery work.
fn load_leftover_at(paths: &[PathBuf]) -> Option<TeardownState> {
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        match serde_json::from_slice(&bytes) {
            Ok(state) => return Some(state),
            Err(e) => {
                tracing::warn!(%e, path = %path.display(), "ignoring unparseable teardown state file");
                if let Err(remove_error) = std::fs::remove_file(path) {
                    tracing::warn!(%remove_error, path = %path.display(), "failed to remove unparseable teardown state file");
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> TeardownState {
        TeardownState {
            interface_name: Some("utun8".to_string()),
            dns_mechanism_applied: Some(dns::Mechanism::Scutil),
            blackholes_added: true,
            bypass_routes: vec![("35.213.7.172".to_string(), "en0".to_string())],
            killswitch_active: true,
            pf_was_enabled: Some(false),
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
            interface_name: Some("wg0_gnosisvpn".to_string()),
            dns_mechanism_applied: None,
            blackholes_added: false,
            bypass_routes: Vec::new(),
            killswitch_active: false,
            pf_was_enabled: None,
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
        value
            .as_object_mut()
            .expect("state serializes to an object")
            .remove("killswitch_active");
        value
            .as_object_mut()
            .expect("state serializes to an object")
            .remove("pf_was_enabled");
        let parsed: TeardownState = serde_json::from_value(value)?;
        assert!(parsed.bypass_routes.is_empty());
        assert!(!parsed.killswitch_active);
        assert_eq!(parsed.pf_was_enabled, None);
        Ok(())
    }

    #[test]
    fn legacy_interface_name_string_remains_compatible() -> anyhow::Result<()> {
        let parsed: TeardownState = serde_json::from_str(
            r#"{"interface_name":"utun8","dns_mechanism_applied":null,"blackholes_added":false}"#,
        )?;
        assert_eq!(parsed.interface_name.as_deref(), Some("utun8"));
        assert!(parsed.is_empty());
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
    fn production_state_path_is_root_owned() {
        let paths = candidate_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/var/run/gnosisvpn/teardown-state.json"));
    }

    #[test]
    fn loading_state_does_not_consume_it_before_cleanup_succeeds() {
        let dir = test_dir("roundtrip");
        let paths = [dir.join("teardown-state.json")];
        let state = sample_state();
        record_at(&paths, &state);
        assert_eq!(load_leftover_at(&paths), Some(state.clone()));
        assert_eq!(load_leftover_at(&paths), Some(state));
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
        assert_eq!(load_leftover_at(&paths), Some(state));
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
        assert_eq!(load_leftover_at(&paths), None);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn recorded_state_is_owner_readable_only() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("perms");
        let path = dir.join("teardown-state.json");
        let paths = [path.clone()];
        record_at(&paths, &sample_state());
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        // A state file left behind with looser permissions is tightened on the
        // next record, not just at creation.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        record_at(&paths, &sample_state());
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn clear_removes_the_state_file() {
        let dir = test_dir("clear");
        let paths = [dir.join("teardown-state.json")];
        record_at(&paths, &sample_state());
        clear_at(&paths);
        assert_eq!(load_leftover_at(&paths), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_one_effect_keeps_other_recovery_work_retryable() {
        let mut state = sample_state();
        state.blackholes_added = false;
        assert!(!state.is_empty());
        assert!(state.killswitch_active);
        assert_eq!(state.dns_mechanism_applied, Some(dns::Mechanism::Scutil));
        assert_eq!(state.bypass_routes.len(), 1);
    }

    #[test]
    fn killswitch_updates_preserve_recorded_routing_effects() {
        let dir = test_dir("merge-killswitch");
        let paths = [dir.join("teardown-state.json")];
        let state = sample_state();
        record_at(&paths, &state);

        record_killswitch_at(&paths, Some(true));

        let merged = load_leftover_at(&paths).expect("merged recovery state");
        assert_eq!(merged.interface_name, state.interface_name);
        assert_eq!(merged.dns_mechanism_applied, state.dns_mechanism_applied);
        assert_eq!(merged.bypass_routes, state.bypass_routes);
        assert!(merged.killswitch_active);
        assert_eq!(merged.pf_was_enabled, Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn successful_routing_cleanup_preserves_killswitch_recovery() {
        let dir = test_dir("clear-routing");
        let paths = [dir.join("teardown-state.json")];
        record_at(&paths, &sample_state());

        clear_routing_at(&paths);

        let remaining = load_leftover_at(&paths).expect("killswitch recovery remains");
        assert_eq!(remaining.interface_name, None);
        assert_eq!(remaining.dns_mechanism_applied, None);
        assert!(!remaining.blackholes_added);
        assert!(remaining.bypass_routes.is_empty());
        assert!(remaining.killswitch_active);
        assert_eq!(remaining.pf_was_enabled, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
