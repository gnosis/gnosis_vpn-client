use std::os::unix::fs::PermissionsExt;
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::dirs;

#[derive(Error, Debug)]
pub enum Error {
    #[error("wg-quick not available")]
    NotAvailable,
    #[error("wg-quick not runnable: {0}")]
    NotRunnable(String),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("Monitoring error: {0}")]
    Monitoring(String),
    #[error("WG generate key error [status: {0}]: {1}")]
    WgGenKey(i32, String),
    #[error("WG quick error [status: {0}]: {1}")]
    WgQuick(i32, String),
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
}

#[derive(Clone, Debug)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub address: String,
    #[allow(dead_code)]
    pub mtu: usize,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct KeyPair {
    pub priv_key: String,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq)]
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
    let code = Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if code.success() {
        Ok(())
    } else {
        Err(Error::NotAvailable)
    }
}

pub async fn executable() -> Result<(), Error> {
    let output = Command::new("wg-quick")
        // suppress stdout
        .stdout(std::process::Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::NotRunnable(format!(
            "wg-quick failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";

async fn generate_key() -> Result<String, Error> {
    let output = Command::new("wg").arg("genkey").output().await?;

    if output.status.success() {
        let key = String::from_utf8(output.stdout).map(|s| s.trim().to_string())?;
        Ok(key)
    } else {
        Err(Error::WgGenKey(
            output.status.code().unwrap_or_default(),
            format!("wg genkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
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

    let output = command.wait_with_output().await?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    } else {
        Err(Error::WgGenKey(
            output.status.code().unwrap_or_default(),
            format!("wg pubkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

impl WireGuard {
    pub async fn from_config(config: Config) -> Result<Self, Error> {
        let priv_key = match config.force_private_key.clone() {
            Some(key) => key,
            None => generate_key().await?,
        };
        let public_key = public_key(&priv_key).await?;
        let key_pair = KeyPair { priv_key, public_key };
        Ok(WireGuard { config, key_pair })
    }

    pub async fn up(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error> {
        let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
        let config = self.to_file_string(interface, peer);
        let content = config.as_bytes();
        fs::write(&conf_file, content).await?;
        fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;

        let output = Command::new("wg-quick").arg("up").arg(conf_file).output().await?;
        if !output.stdout.is_empty() {
            tracing::info!("wg-quick up stdout: {}", String::from_utf8_lossy(&output.stdout));
        }

        if output.status.success() {
            if !output.stderr.is_empty() {
                // wg-quick populates stderr with info and warnings, log those in debug mode
                tracing::debug!("wg-quick up stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
            Ok(())
        } else {
            Err(Error::WgQuick(
                output.status.code().unwrap_or_default(),
                format!("wg-quick up failed: {}", String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }

    fn to_file_string(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> String {
        let allowed_ips = match &self.config.allowed_ips {
            Some(allowed_ips) => allowed_ips.clone(),
            None => interface.address.split('.').take(2).collect::<Vec<&str>>().join(".") + ".0.0/9",
        };
        let listen_port_line = self
            .config
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        // WireGuard has differently sized packets not exactly adhering to MTU
        // so we postpone optimizing on this level for now
        // MTU = {mtu}
        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
{listen_port_line}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = {allowed_ips}
",
            private_key = self.key_pair.priv_key,
            address = interface.address,
            public_key = peer.public_key,
            endpoint = peer.endpoint,
            allowed_ips = allowed_ips,
            listen_port_line = listen_port_line,
            // WireGuard has differnently sized packets not exactly adhering to MTU
            // so we postpone optimizing on this level for now
            // mtu = interface.mtu,
        )
    }
}

pub async fn down() -> Result<(), Error> {
    let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;

    let output = Command::new("wg-quick").arg("down").arg(conf_file).output().await?;
    if !output.stdout.is_empty() {
        tracing::info!("wg-quick down stdout: {}", String::from_utf8_lossy(&output.stdout));
    }

    if output.status.success() {
        if !output.stderr.is_empty() {
            // wg-quick populates stderr with info and warnings, log those in debug mode
            tracing::debug!("wg-quick down stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    } else {
        Err(Error::WgQuick(
            output.status.code().unwrap_or_default(),
            format!("wg-quick down failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}
