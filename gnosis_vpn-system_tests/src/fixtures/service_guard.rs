use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::Context;
use gnosis_vpn_lib::network::Network;

use crate::fixtures::system_test_config::SystemTestConfig;

pub struct ServiceGuard {
    child: Child,
}

impl ServiceGuard {
    pub fn spawn(binary: &Path, cfg: &SystemTestConfig, socket_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).context("create socket directory")?;
        }

        let mut cmd = Command::new(binary);
        cmd.arg("--hopr-rpc-provider")
            .arg(cfg.rpc_provider.as_str())
            .arg("--hopr-network")
            .arg(Network::Rotsee.to_string())
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

    #[allow(dead_code)]
    pub fn ensure_running(&mut self) -> anyhow::Result<()> {
        if let Some(status) = self.child.try_wait()? {
            Err(anyhow::anyhow!("gnosis_vpn exited early with status {status}"))?;
        }
        Ok(())
    }
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
