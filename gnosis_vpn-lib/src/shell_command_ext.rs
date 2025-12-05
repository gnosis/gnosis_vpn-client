use thiserror::Error;
use tokio::process::Command;

use std::io;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Command not executable")]
    NotExecutable,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

pub trait ShellCommandExt {
    async fn run(&mut self) -> Result<(), Error>;
    async fn run_stdout(&mut self) -> Result<String, Error>;
}

impl ShellCommandExt for Command {
    async fn run(&mut self) -> Result<(), Error> {
        let output = self.output().await?;
        let stderrempty = output.stderr.is_empty();
        match (stderrempty, output.status) {
            (true, status) if status.success() => Ok(()),
            (false, status) if status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(cmd = ?self, %stderr, "Non empty stderr on successful command");
                Ok(())
            }
            (_, status) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!(cmd = ?self, status_code = ?status.code(), %stdout, %stderr, "Error executing command");
                Err(Error::NotExecutable)
            }
        }
    }

    async fn run_stdout(&mut self) -> Result<String, Error> {
        let output = self.output().await?;
        let stderrempty = output.stderr.is_empty();
        let stdout = String::from_utf8_lossy(&output.stdout);
        match (stderrempty, output.status) {
            (true, status) if status.success() => Ok(stdout.trim().to_string()),
            (false, status) if status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(cmd = ?self, %stderr, "Non empty stderr on successful command");
                Ok(stdout.trim().to_string())
            }
            (_, status) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!(cmd = ?self, status_code = ?status.code(), %stdout, %stderr, "Error executing command");
                Err(Error::NotExecutable)
            }
        }
    }
}
