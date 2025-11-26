use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::cli::CliArgs;
use anyhow::Context;

/// Owns the spawned gnosis_vpn service process and tears it down when dropped.
pub struct ServiceGuard {
    child: Child,
}

impl ServiceGuard {
    /// Spawns the binary with the configuration required for system tests.
    pub fn spawn(binary: &Path, cfg: &CliArgs, socket_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).context("create socket directory")?;
        }

        let mut cmd = Command::new(binary);
        cmd.arg("--hopr-rpc-provider")
            .arg(cfg.rpc_provider.as_str())
            .arg("--hopr-network")
            .arg(cfg.network.as_str())
            .arg("--socket-path")
            .arg(socket_path.as_os_str())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        if cfg.allow_insecure {
            cmd.arg("--allow-insecure");
        }

        let child = cmd.spawn().context("failed to start gnosis_vpn service")?;
        Ok(Self { child })
    }
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        // Ensure we always stop the background service at the end of the test run.
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
