use thiserror::Error;
use tokio::process::Command;

use std::future::Future;
use std::io;
use std::process::Output;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Command execution failed")]
    CommandFailed,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

/// log errors and warnings or suppress them
#[derive(Debug)]
pub enum Logs {
    Print,
    Suppress,
}

pub trait ShellCommandExt {
    fn run(&mut self, logs: Logs) -> impl Future<Output = Result<(), Error>> + Send;
    fn run_stdout(&mut self, logs: Logs) -> impl Future<Output = Result<String, Error>> + Send;
    fn spawn_no_capture(&mut self) -> impl Future<Output = Result<(), Error>> + Send;
}

impl ShellCommandExt for Command {
    /// Run the command and print stderr with a warning on success.
    /// Unconditionally captures stdout and stderr regardless of command settings.
    /// See tokio's output behaviour: https://docs.rs/tokio/latest/tokio/process/struct.Command.html#method.output
    async fn run(&mut self, logs: Logs) -> Result<(), Error> {
        let output = self.output().await?;
        let stderrempty = output.stderr.is_empty();
        match (stderrempty, output.status) {
            (true, status) if status.success() => Ok(()),
            (false, status) if status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if matches!(logs, Logs::Print) {
                    tracing::warn!(cmd = ?self, %stderr, "Non empty stderr on successful command");
                }
                Ok(())
            }
            (_, status) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if matches!(logs, Logs::Print) {
                    tracing::error!(cmd = ?self, status_code = ?status.code(), %stdout, %stderr, "Error executing command");
                }
                Err(Error::CommandFailed)
            }
        }
    }

    async fn run_stdout(&mut self, logs: Logs) -> Result<String, Error> {
        let output = self.output().await?;
        let cmd_debug = format!("{:?}", self);
        stdout_from_output(cmd_debug, output, logs)
    }

    async fn spawn_no_capture(&mut self) -> Result<(), Error> {
        let mut cmd = self
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        match cmd.wait().await {
            Ok(status) if status.success() => Ok(()),
            _ => Err(Error::CommandFailed),
        }
    }
}

pub fn stdout_from_output(cmd: String, output: Output, logs: Logs) -> Result<String, Error> {
    let stderrempty = output.stderr.is_empty();
    let stdout = String::from_utf8_lossy(&output.stdout);
    match (stderrempty, output.status) {
        (true, status) if status.success() => Ok(stdout.trim().to_string()),
        (false, status) if status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if matches!(logs, Logs::Print) {
                tracing::warn!(cmd, %stderr, "Non empty stderr on successful command");
            }
            Ok(stdout.trim().to_string())
        }
        (_, status) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if matches!(logs, Logs::Print) {
                tracing::error!(cmd, status_code = ?status.code(), %stdout, %stderr, "Error executing command");
            }
            Err(Error::CommandFailed)
        }
    }
}
