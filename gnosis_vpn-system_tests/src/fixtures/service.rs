use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::cli::SharedArgs;
use anyhow::Context;
use tracing::info;

pub struct Service;

impl Service {
    /// Spawns the binary with the configuration required for system tests.
    pub fn spawn(binary: &Path, cfg: &SharedArgs, socket_path: &Path) -> anyhow::Result<ServiceGuard> {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).context("create socket directory")?;
        }

        let mut cmd = Command::new(binary);
        cmd.arg("--hopr-blokli-url")
            .arg(cfg.blokli_url.as_str())
            .arg("--socket-path")
            .arg(socket_path.as_os_str())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        if cfg.allow_insecure {
            cmd.arg("--allow-insecure");
        }

        info!("Spawning gnosis-vpn service with command: {:?}", cmd);

        match cmd.spawn() {
            Ok(_child) => {
                info!("Started gnosis-vpn service");
                Ok(ServiceGuard(_child))
            }
            Err(error) => Err(error.into()),
        }
    }
}

pub struct ServiceGuard(Child);

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
