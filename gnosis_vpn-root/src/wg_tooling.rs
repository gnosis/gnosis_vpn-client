use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::os::unix::fs::PermissionsExt;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;

use crate::dirs;

pub async fn available() -> Result<(), Error> {
    Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| Error::NotAvailable("wg-quick".to_string()))
}

pub async fn executable() -> Result<(), Error> {
    Command::new("wg-quick")
        .arg("-h")
        // suppress stdout
        .stdout(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| Error::NotExecutable("wg-quick".to_string()))
}

pub async fn up(config_content: String) -> Result<(), Error> {
    let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
    let content = config_content.as_bytes();
    fs::write(&conf_file, content).await?;
    fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;
    Command::new("wg-quick").arg("up").arg(conf_file).run().await
}

pub async fn down() -> Result<(), Error> {
    let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
    Command::new("wg-quick").arg("down").arg(conf_file).run().await
}
