use tokio::fs;
use tokio::process::Command;

use std::os::unix::fs::PermissionsExt;
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
    fs::write(&conf_file, content).await?;
    fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;
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
