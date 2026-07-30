use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{dirs, wireguard};

pub async fn available() -> Result<(), wireguard::Error> {
    let out = Command::new("which")
        .arg("wg-quick")
        .run_stdout(Logs::Print)
        .await
        .map_err(wireguard::Error::from)?;
    tracing::debug!(at = %out, "wg-quick command available");
    Ok(())
}

pub async fn executable() -> Result<(), wireguard::Error> {
    Command::new("wg-quick")
        .arg("-h")
        .spawn_no_capture()
        .await
        .map_err(wireguard::Error::from)
}

/// Path of the WireGuard config handed to `wg-quick`.
///
/// Lives in the state home root, not in the cache directory: the cache directory is `0700` and
/// owned by the worker user, while `wg-quick` runs as root without a DAC-bypass capability
/// (distro AppArmor profiles deny `dac_override`, and `CAP_DAC_READ_SEARCH` is not in the
/// service's capability bounding set). It can therefore only reach the config through
/// traversable directories, reading it as the file's owner.
fn config_path(state_home: PathBuf) -> PathBuf {
    state_home.join(wireguard::WG_CONFIG_FILE)
}

/// Write the WireGuard config to a file and bring up the interface using `wg-quick`.
/// Returns created interface name on success.
pub async fn up(state_home: PathBuf, config_content: String) -> Result<String, wireguard::Error> {
    let conf_file = config_path(state_home.clone());
    let content = config_content.as_bytes();

    // The config used to live in the cache directory - drop that copy so the private key it
    // holds does not linger there.
    let _ = fs::remove_file(dirs::cache_dir(state_home, wireguard::WG_CONFIG_FILE)).await;

    // Remove stale config so mode() applies to a fresh file (O_CREAT only sets mode on creation)
    let _ = fs::remove_file(&conf_file).await;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&conf_file)
        .await?;
    file.write_all(content).await?;
    file.flush().await?;

    Command::new("wg-quick")
        .arg("up")
        .arg(conf_file)
        .run(Logs::Print)
        .await?;

    let iface_name = resolve_interface_name().await;
    Ok(iface_name)
}

/// Resolve the real WireGuard interface name.
///
/// On macOS, `wg-quick` creates `utunN` interfaces and stores the mapping in
/// `/var/run/wireguard/<config>.name`. On Linux, the interface name matches the
/// config name directly.
pub async fn resolve_interface_name() -> String {
    #[cfg(target_os = "macos")]
    {
        let name_file = format!("/var/run/wireguard/{}.name", wireguard::WG_INTERFACE);
        match fs::read_to_string(&name_file).await {
            Ok(name) => {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    tracing::debug!(interface = %name, "resolved WireGuard interface name");
                    return name;
                }
            }
            Err(e) => {
                tracing::warn!(%e, path = %name_file,
                    "could not read WG interface name file, using default");
            }
        }
    }
    wireguard::WG_INTERFACE.to_string()
}

pub async fn down(state_home: PathBuf, logs: Logs) -> Result<(), wireguard::Error> {
    let conf_file = config_path(state_home);
    Command::new("wg-quick").arg("down").arg(conf_file).run(logs).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── config_path ──────────────────────────────────────────────────────────

    #[test]
    fn config_path_resolves_in_state_home_root() {
        let path = config_path(PathBuf::from("/var/lib/gnosisvpn"));
        assert_eq!(path, PathBuf::from("/var/lib/gnosisvpn/wg0_gnosisvpn.conf"));
    }

    // wg-quick has no DAC-bypass capability, so it cannot traverse the 0700 worker-owned
    // cache directory.
    #[test]
    fn config_path_is_outside_the_cache_directory() {
        let state_home = PathBuf::from("/var/lib/gnosisvpn");
        let path = config_path(state_home.clone());
        assert_ne!(path, dirs::cache_dir(state_home, wireguard::WG_CONFIG_FILE));
        assert_eq!(path.parent(), Some(PathBuf::from("/var/lib/gnosisvpn").as_path()));
    }
}
