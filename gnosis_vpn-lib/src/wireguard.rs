use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::fmt::{self, Display};
use std::{io, string};

use crate::dirs;
use crate::shell_command_ext::{self, Logs, ShellCommandExt};

pub const WG_INTERFACE: &str = "wg0_gnosisvpn";
pub const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";
pub const WG_MTU: u32 = 1420;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
    #[error("UTF8 conversion error: {0}")]
    FromUtf8Error(#[from] string::FromUtf8Error),
    #[error("TOML serialization error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("error generating wg key")]
    WgGenKey,
    #[error("Dirs error: {0}")]
    Dirs(#[from] dirs::Error),
    #[error("Shell command error: {0}")]
    ShellCommandExt(#[from] shell_command_ext::Error),
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub address: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub public_key: String,
    pub preshared_key: String,
    pub endpoint: String,
}

impl fmt::Debug for PeerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerInfo")
            .field("public_key", &self.public_key)
            .field("preshared_key", &"****")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyPair {
    pub priv_key: String,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub force_private_key: Option<String>,
    pub allowed_ips: Option<String>,
    pub dns: Option<String>,
}

impl Config {
    pub(crate) fn new(
        listen_port: Option<u16>,
        allowed_ips: Option<String>,
        force_private_key: Option<String>,
        dns: Option<String>,
    ) -> Self {
        Config {
            listen_port,
            allowed_ips,
            force_private_key,
            dns,
        }
    }
}

pub async fn available() -> Result<(), Error> {
    let out = Command::new("which")
        .arg("wg")
        .run_stdout(Logs::Print)
        .await
        .map_err(Error::from)?;
    tracing::debug!(at = %out, "wg command available");
    Ok(())
}

pub async fn executable() -> Result<(), Error> {
    Command::new("wg")
        .arg("--version")
        .spawn_no_capture()
        .await
        .map_err(Error::from)
}

async fn generate_key() -> Result<String, Error> {
    Command::new("wg")
        .arg("genkey")
        .run_stdout(Logs::Print)
        .await
        .map_err(|_| Error::WgGenKey)
}

async fn public_key(priv_key: &str) -> Result<String, Error> {
    let mut command = Command::new("wg")
        .arg("pubkey")
        .stdin(std::process::Stdio::piped()) // Enable piping to stdin
        .stdout(std::process::Stdio::piped()) // Capture stdout
        .spawn()?;

    if let Some(stdin) = command.stdin.as_mut() {
        stdin.write_all(priv_key.as_bytes()).await?
    }

    let cmd_debug = format!("{:?}", command);
    let output = command.wait_with_output().await?;
    shell_command_ext::stdout_from_output(cmd_debug, output, Logs::Print).map_err(|_| Error::WgGenKey)
}

impl WireGuard {
    pub fn new(config: Config, key_pair: KeyPair) -> Self {
        WireGuard { config, key_pair }
    }

    pub async fn from_config(config: Config) -> Result<Self, Error> {
        let priv_key = match config.force_private_key.clone() {
            Some(key) => key,
            None => generate_key().await?,
        };
        let public_key = public_key(&priv_key).await?;
        let key_pair = KeyPair { priv_key, public_key };
        Ok(WireGuard { config, key_pair })
    }

    /// Render the configuration in `wg setconf` format.
    ///
    /// Only keys understood by `wg(8)` are emitted (PrivateKey, ListenPort and
    /// the [Peer] block). Interface properties like address, MTU, DNS and
    /// routing are applied natively by the platform tooling in gnosis_vpn-root.
    pub fn to_setconf_string(&self, peer: &PeerInfo) -> String {
        let allowed_ips = self.config.allowed_ips.clone().unwrap_or("0.0.0.0/0".to_string());
        let mut lines = Vec::new();

        lines.push("[Interface]".to_string());
        lines.push(format!("PrivateKey = {}", self.key_pair.priv_key));
        if let Some(listen_port) = self.config.listen_port {
            lines.push(format!("ListenPort = {}", listen_port));
        }

        lines.push("".to_string()); // Empty line for spacing

        lines.push("[Peer]".to_string());
        lines.push(format!("PublicKey = {}", peer.public_key));
        lines.push(format!("PresharedKey = {}", peer.preshared_key));
        lines.push(format!("Endpoint = {}", peer.endpoint));
        lines.push(format!("AllowedIPs = {}", allowed_ips));

        lines.join("\n")
    }
}

impl Display for WireGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WireGuard {{ public_key: {} }}", self.key_pair.public_key)
    }
}

impl fmt::Debug for WireGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WireGuard {{ public_key: {} }}", self.key_pair.public_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wireguard(listen_port: Option<u16>, allowed_ips: Option<String>) -> WireGuard {
        WireGuard {
            config: Config {
                listen_port,
                force_private_key: None,
                allowed_ips,
                dns: Some("1.1.1.1,8.8.8.8".to_string()),
            },
            key_pair: KeyPair {
                priv_key: "PRIV_KEY".to_string(),
                public_key: "PUB_KEY".to_string(),
            },
        }
    }

    fn peer() -> PeerInfo {
        PeerInfo {
            public_key: "SERVER_PUB_KEY".to_string(),
            preshared_key: "PRESHARED_KEY".to_string(),
            endpoint: "1.2.3.4:51820".to_string(),
        }
    }

    #[test]
    fn setconf_contains_only_wg_keys() {
        let content = wireguard(None, None).to_setconf_string(&peer());
        assert_eq!(
            content,
            "[Interface]\n\
             PrivateKey = PRIV_KEY\n\
             \n\
             [Peer]\n\
             PublicKey = SERVER_PUB_KEY\n\
             PresharedKey = PRESHARED_KEY\n\
             Endpoint = 1.2.3.4:51820\n\
             AllowedIPs = 0.0.0.0/0"
        );
    }

    #[test]
    fn setconf_includes_listen_port_when_set() {
        let content = wireguard(Some(51821), None).to_setconf_string(&peer());
        assert!(content.contains("ListenPort = 51821"));
    }

    #[test]
    fn setconf_uses_configured_allowed_ips() {
        let content = wireguard(None, Some("10.128.0.0/9".to_string())).to_setconf_string(&peer());
        assert!(content.contains("AllowedIPs = 10.128.0.0/9"));
        assert!(!content.contains("0.0.0.0/0"));
    }

    #[test]
    fn setconf_never_emits_interface_properties() {
        // Address/MTU/DNS/Table are wg-quick keys; `wg setconf` rejects them.
        let content = wireguard(Some(51821), None).to_setconf_string(&peer());
        for key in ["Address", "MTU", "DNS", "Table", "PreUp", "PostDown"] {
            assert!(!content.contains(key), "unexpected key in setconf output: {key}");
        }
    }
}
