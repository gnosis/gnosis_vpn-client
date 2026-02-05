use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::cli::SharedArgs;
use anyhow::Context;
use tracing::info;

pub struct Service;

impl Service {
    /// Spawns the binary with the configuration required for system tests.
    pub fn spawn(binary: &Path, cfg: &SharedArgs, socket_path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).context("create socket directory")?;
        }

        let mut cmd = Command::new(binary);
        cmd.arg("--hopr-blokli-url")
            .arg(cfg.blokli_url.as_str())
            .arg("--socket-path")
            .arg(socket_path.as_os_str())
            .arg("--worker-binary")
            .arg(cfg.worker_binary.as_os_str())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        if cfg.allow_insecure {
            cmd.arg("--allow-insecure");
        }

        match cmd.spawn() {
            Ok(_child) => {
                info!("Started gnosis-vpn service");
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}
