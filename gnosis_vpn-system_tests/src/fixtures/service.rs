use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli::SharedArgs;
use anyhow::Context;
use rustix::process::{Pid, Signal, kill_process};
use tracing::{info, warn};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

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

        if cfg.allow_experimental {
            cmd.arg("--allow-experimental");
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
    // SIGKILL skips the root's routing teardown and leaves the runner offline behind a dead tunnel
    fn drop(&mut self) {
        let pid = Pid::from_child(&self.0);
        if let Err(error) = kill_process(pid, Signal::TERM) {
            warn!(?error, "failed to send SIGTERM to gnosis-vpn service");
        }

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        warn!(timeout = ?SHUTDOWN_TIMEOUT, "gnosis-vpn service did not stop, killing it");
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
