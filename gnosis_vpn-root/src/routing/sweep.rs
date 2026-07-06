//! Crash-recovery sweep for tunnel side effects that survive an unclean root exit.
//!
//! Routes bound to the TUN interface vanish with root's fd when the process dies,
//! but the IPv6 blackhole routes and the DNS diversion (resolvectl/resolvconf on
//! Linux, the `State:/Network/Service/<utunN>/DNS` scutil key on macOS) do not.
//! Tunnel setup records them in a small state file which clean teardown deletes;
//! if the file is still present at the next daemon start, the recorded side
//! effects are removed best-effort.

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
    let Some(state) = take_leftover_at(&candidate_paths()) else {
        return;
    };
    tracing::info!(
        interface = %state.interface_name,
        dns_mechanism = ?state.dns_mechanism_applied,
        blackholes = state.blackholes_added,
        "found teardown state from an unclean exit - sweeping leftover tunnel side effects"
    );
    if state.blackholes_added {
        ipv6_blackhole::remove().await;
    }
    if let Some(mechanism) = state.dns_mechanism_applied {
        dns::restore(&state.interface_name, mechanism).await;
    }
    tracing::info!("crash-recovery sweep complete");
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
        };
        let json = serde_json::to_string(&state)?;
        let parsed: TeardownState = serde_json::from_str(&json)?;
        assert_eq!(parsed, state);
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
