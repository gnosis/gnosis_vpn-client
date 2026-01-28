use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::fmt::{self, Display};
use std::{io, string};

use crate::dirs;
use crate::shell_command_ext::{self, ShellCommandExt, Logs};

pub const WG_INTERFACE: &str = "wg0_gnosisvpn";
pub const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
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
}

impl Config {
    pub(crate) fn new<L, M, S>(listen_port: Option<L>, allowed_ips: Option<M>, force_private_key: Option<S>) -> Self
    where
        L: Into<u16>,
        M: Into<String>,
        S: Into<String>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
            allowed_ips: allowed_ips.map(Into::into),
            force_private_key: force_private_key.map(Into::into),
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

    pub fn to_file_string(
        &self,
        interface: &InterfaceInfo,
        peer: &PeerInfo,
        route_all_traffic: bool,
        extra_interface_lines: Option<Vec<String>>,
    ) -> String {
        let allowed_ips = match (route_all_traffic, &self.config.allowed_ips) {
            (true, _) => "0.0.0.0/0".to_string(),
            (_, Some(allowed_ips)) => allowed_ips.clone(),
            _ => interface.address.split('.').take(2).collect::<Vec<&str>>().join(".") + ".0.0/9",
        };

        let mut lines = Vec::new();

        // [Interface] section
        lines.push("[Interface]".to_string());
        lines.push(format!("PrivateKey = {}", self.key_pair.priv_key));
        lines.push(format!("Address = {}", interface.address));
        if let Some(listen_port) = self.config.listen_port {
            lines.push(format!("ListenPort = {}", listen_port));
        }
        if let Some(extra_lines) = extra_interface_lines {
            lines.extend(extra_lines);
        }

        lines.push("".to_string()); // Empty line for spacing

        // [Peer] section
        lines.push("[Peer]".to_string());
        lines.push(format!("PublicKey = {}", peer.public_key));
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
