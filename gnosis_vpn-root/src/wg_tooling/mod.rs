//! Native WireGuard interface management, replacing `wg-quick`.
//!
//! The platform modules bring the tunnel up in the same order `wg-quick` did:
//! IPv6 blackholes → create interface → `wg setconf` → address → MTU → up → DNS.
//! Down is the reverse. Routing itself is owned by `crate::routing`.

use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext;
use gnosis_vpn_lib::{dirs, event, wireguard};

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
        pub use linux::{down, up};
    } else if #[cfg(target_os = "macos")] {
        mod macos;
        pub use macos::{down, up};
    }
}

/// Verify the external tools needed for WireGuard bring-up are installed.
///
/// Both platforms need `wg` for setconf and key handling. Linux DNS goes
/// through `resolvconf`, so it is required only when the effective config
/// enables DNS — checked at startup to fail fast instead of at connect time.
/// macOS needs `wireguard-go` to create the utun device; DNS uses the
/// always-present `networksetup`.
pub async fn check(dns_enabled: bool) -> Result<(), Error> {
    use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;

    check_command("wg").await?;
    tokio::process::Command::new("wg")
        .arg("--version")
        .spawn_no_capture()
        .await?;

    cfg_if::cfg_if! {
        if #[cfg(target_os = "linux")] {
            if dns_enabled {
                check_command("resolvconf").await?;
            }
        } else if #[cfg(target_os = "macos")] {
            let _ = dns_enabled;
            check_command("wireguard-go").await?;
        }
    }
    Ok(())
}

async fn check_command(command: &str) -> Result<(), Error> {
    use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
    let out = tokio::process::Command::new("which")
        .arg(command)
        .run_stdout(Logs::Print)
        .await?;
    tracing::debug!(at = %out, %command, "wireguard tooling available");
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("shell command error: {0}")]
    ShellCommand(#[from] shell_command_ext::Error),
    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0}")]
    Rtnetlink(#[from] rtnetlink::Error),
    #[error("{0}")]
    General(String),
}

/// Parse an interface address like "10.128.0.5/32" into (addr, prefix_len).
/// A bare address defaults to /32.
fn parse_address(address: &str) -> Result<(std::net::Ipv4Addr, u8), Error> {
    let (addr_str, prefix_str) = match address.split_once('/') {
        Some((addr, prefix)) => (addr, prefix),
        None => (address, "32"),
    };
    let addr = addr_str
        .parse()
        .map_err(|e| Error::General(format!("invalid interface address '{address}': {e}")))?;
    let prefix = prefix_str
        .parse()
        .map_err(|e| Error::General(format!("invalid interface address prefix '{address}': {e}")))?;
    Ok((addr, prefix))
}

/// Write the `wg setconf` file with owner-only permissions and return its path.
/// The file contains the private key, so it must never be world readable
/// and its content must never appear on a command line.
async fn write_setconf_file(state_home: PathBuf, wg_data: &event::WireGuardData) -> Result<PathBuf, Error> {
    let conf_file = dirs::cache_dir(state_home, wireguard::WG_CONFIG_FILE);
    let content = wg_data.wg.to_setconf_string(&wg_data.peer_info);

    // Remove stale config so mode() applies to a fresh file (O_CREAT only sets mode on creation)
    let _ = fs::remove_file(&conf_file).await;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&conf_file)
        .await?;
    file.write_all(content.as_bytes()).await?;
    file.flush().await?;
    Ok(conf_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_address_with_prefix() {
        let (addr, prefix) = parse_address("10.128.0.5/32").unwrap();
        assert_eq!(addr, std::net::Ipv4Addr::new(10, 128, 0, 5));
        assert_eq!(prefix, 32);
    }

    #[test]
    fn parse_address_bare_defaults_to_slash32() {
        let (addr, prefix) = parse_address("10.128.0.5").unwrap();
        assert_eq!(addr, std::net::Ipv4Addr::new(10, 128, 0, 5));
        assert_eq!(prefix, 32);
    }

    #[test]
    fn parse_address_rejects_garbage() {
        assert!(parse_address("not-an-ip").is_err());
        assert!(parse_address("10.0.0.1/xx").is_err());
    }
}
