use tokio::fs;
use tokio::process::Command;

use std::os::unix::fs::PermissionsExt;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{dirs, wireguard};

pub async fn available() -> Result<(), wireguard::Error> {
    Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| wireguard::Error::NotAvailable("wg-quick".to_string()))
}

pub async fn executable() -> Result<(), wireguard::Error> {
    Command::new("wg-quick")
        .arg("-h")
        // suppress stdout and stderr
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| wireguard::Error::NotExecutable("wg-quick".to_string()))
}

pub async fn up(config_content: String) -> Result<(), wireguard::Error> {
    let conf_file = dirs::cache_dir(wireguard::WG_CONFIG_FILE)?;
    let content = config_content.as_bytes();
    fs::write(&conf_file, content).await?;
    fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;
    Command::new("wg-quick").arg("up").arg(conf_file).run().await?;
    Ok(())
}

pub async fn down() -> Result<(), wireguard::Error> {
    let conf_file = dirs::cache_dir(wireguard::WG_CONFIG_FILE)?;
    Command::new("wg-quick").arg("down").arg(conf_file).run().await?;
    Ok(())
}
