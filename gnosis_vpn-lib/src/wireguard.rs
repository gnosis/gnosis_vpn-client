use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::fmt::{self, Display};
use std::{io, string};

use crate::dirs;
use crate::shell_command_ext::{self, ShellCommandExt};

pub const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] io::Error),
    #[error(transparent)]
    FromUtf8Error(#[from] string::FromUtf8Error),
    #[error(transparent)]
    Toml(#[from] toml::ser::Error),
    #[error("error generating wg key")]
    WgGenKey,
    #[error(transparent)]
    Dirs(#[from] dirs::Error),
    #[error(transparent)]
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
    Command::new("which")
        .arg("wg")
        .spawn_no_capture()
        .await
        .map_err(Error::from)
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
        .run_stdout()
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
    shell_command_ext::stdout_from_output(cmd_debug, output).map_err(|_| Error::WgGenKey)
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

    pub fn to_file_string(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> String {
        let listen_port_line = self
            .config
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
{listen_port_line}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = 0.0.0.0/0
",
            private_key = self.key_pair.priv_key,
            address = interface.address,
            public_key = peer.public_key,
            endpoint = peer.endpoint,
            listen_port_line = listen_port_line,
        )
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
