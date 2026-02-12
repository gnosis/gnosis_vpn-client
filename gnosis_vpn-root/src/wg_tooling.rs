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

pub async fn up(state_home: PathBuf, config_content: String) -> Result<(), wireguard::Error> {
    let conf_file = dirs::cache_dir(state_home, wireguard::WG_CONFIG_FILE)?;
    let content = config_content.as_bytes();

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
    Ok(())
}

pub async fn down(state_home: PathBuf, logs: Logs) -> Result<(), wireguard::Error> {
    let conf_file = dirs::cache_dir(state_home, wireguard::WG_CONFIG_FILE)?;
    Command::new("wg-quick").arg("down").arg(conf_file).run(logs).await?;
    Ok(())
}
