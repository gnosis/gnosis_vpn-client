use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;

use crate::dirs;

#[derive(Error, Debug)]
pub enum Error {
    #[error("dependency not available: {0}")]
    NotAvailable(String),
    #[error("dependency {0} not executable: {1}")]
    NotExecutable(String, String),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("WG generate key error [status: {0}]: {1}")]
    WgGenKey(i32, String),
    #[error("WG quick error [status: {0}]: {1}")]
    WgQuick(i32, String),
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error("Unable running network routing detection")]
    RoutingDetection,
}

#[derive(Clone, Debug)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub gateway: Option<String>,
    pub device: String,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub port: u16,
    pub relayer_ip: Ipv4Addr,
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
}

impl Config {
    pub(crate) fn new<L, S>(listen_port: Option<L>, force_private_key: Option<S>) -> Self
    where
        L: Into<u16>,
        S: Into<String>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
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
        Err(Error::NotAvailable("wg-quick".to_string()))
    }
}

pub async fn executable() -> Result<(), Error> {
    let output = Command::new("wg-quick")
        .arg("-h")
        // suppress stdout
        .stdout(std::process::Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::NotExecutable(
            "wg-quick".to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
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

    pub async fn up(&self, address: Ipv4Addr, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error> {
        let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
        let config = self.to_file_string(address, interface, peer);
        let content = config.as_bytes();
        fs::write(&conf_file, content).await?;
        fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;

        let output = Command::new("wg-quick").arg("up").arg(conf_file).output().await?;
        if !output.stdout.is_empty() {
            tracing::info!(
                stdout = String::from_utf8_lossy(&output.stdout).to_string(),
                "unexpected wg-quick up output"
            );
        }

        if output.status.success() {
            if !output.stderr.is_empty() {
                // wg-quick populates stderr with info and warnings, log those in debug mode
                tracing::debug!(
                    stderr = String::from_utf8_lossy(&output.stderr).to_string(),
                    "wg-quick up output"
                );
            }
            Ok(())
        } else {
            Err(Error::WgQuick(
                output.status.code().unwrap_or_default(),
                format!("wg-quick up failed: {}", String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }

    fn to_file_string(&self, address: Ipv4Addr, interface: &InterfaceInfo, peer: &PeerInfo) -> String {
        let listen_port_line = self
            .config
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}/32
PreUp = {pre_up_routing}
PostDown = {post_down_routing}
{listen_port_line}

[Peer]
PublicKey = {public_key}
Endpoint = 127.0.0.1:{port}
AllowedIPs = 0.0.0.0/0
",
            private_key = self.key_pair.priv_key,
            address = address,
            public_key = peer.public_key,
            port = peer.port,
            listen_port_line = listen_port_line,
            pre_up_routing = pre_up_routing(&peer.relayer_ip, interface),
            post_down_routing = post_down_routing(&peer.relayer_ip, interface),
        )
    }
}

impl InterfaceInfo {
    pub async fn from_system(relayer_ip: &Ipv4Addr) -> Result<Self, Error> {
        if cfg!(target_os = "macos") {
            Self::from_macos(relayer_ip).await
        } else {
            // assuming linux
            Self::from_linux(relayer_ip).await
        }
    }

    async fn from_macos(relayer_ip: &Ipv4Addr) -> Result<Self, Error> {
        let output = Command::new("route")
            .arg("-n")
            .arg("get")
            .arg(relayer_ip.to_string())
            .output()
            .await?;
        if !output.stderr.is_empty() {
            tracing::error!(
                stderr = String::from_utf8_lossy(&output.stderr).to_string(),
                "error running route -n get"
            );
        }
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = stdout.split_whitespace().collect();

            let device_index = parts.iter().position(|&x| x == "interface");
            let via_index = parts.iter().position(|&x| x == "gateway");

            let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
                Some(dev) => dev.to_string(),
                None => {
                    tracing::error!(%stdout, "Unable to determine default interface from route -n get output");
                    return Err(Error::NoInterface);
                }
            };

            let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
            Ok(InterfaceInfo { gateway, device })
        } else {
            Err(Error::RoutingDetection)
        }
    }

    async fn from_linux(relayer_ip: &Ipv4Addr) -> Result<Self, Error> {
        let output = Command::new("ip")
            .arg("route")
            .arg("get")
            .arg(relayer_ip.to_string())
            .output()
            .await?;
        if !output.stderr.is_empty() {
            tracing::error!(stderr = String::from_utf8_lossy(&output.stderr).to_string(), %relayer_ip, "error running ip route get");
        }
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = stdout.split_whitespace().collect();

            let device_index = parts.iter().position(|&x| x == "dev");
            let via_index = parts.iter().position(|&x| x == "via");

            let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
                Some(dev) => dev.to_string(),
                None => {
                    tracing::error!(%stdout, %relayer_ip, "Unable to determine default interface from ip route get output");
                    return Err(Error::NoInterface);
                }
            };

            let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
            Ok(InterfaceInfo { gateway, device })
        } else {
            Err(Error::RoutingDetection)
        }
    }
}

pub async fn down() -> Result<(), Error> {
    let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;

    let output = Command::new("wg-quick").arg("down").arg(conf_file).output().await?;
    if !output.stdout.is_empty() {
        tracing::info!(
            stdout = String::from_utf8_lossy(&output.stdout).to_string(),
            "unexpected wg-quick down stdout"
        );
    }

    if output.status.success() {
        if !output.stderr.is_empty() {
            // wg-quick populates stderr with info and warnings, log those in debug mode
            tracing::debug!(
                stderr = String::from_utf8_lossy(&output.stderr).to_string(),
                "wg-quick down output"
            );
        }
        Ok(())
    } else {
        Err(Error::WgQuick(
            output.status.code().unwrap_or_default(),
            format!("wg-quick down failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        // macos
        if let Some(ref gateway) = interface.gateway {
            format!(
                "route -n add --host {relayer_ip} {gateway}",
                relayer_ip = relayer_ip,
                gateway = gateway
            )
        } else {
            format!(
                "route -n add -host {relayer_ip} -interface {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route add {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route add {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        // macos
        format!("route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route del {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route del {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
}
