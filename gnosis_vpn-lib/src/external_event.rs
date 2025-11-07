use std::fmt::{self, Debug, Display};
use std::path::PathBuf;
use tokio::sync::oneshot;

use crate::command::{Command, Response};

/// These events indicate outside requests to the core application loop.
pub enum ExternalEvent {
    Command {
        cmd: Command,
        resp: oneshot::Sender<Response>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
    ConfigReload {
        path: PathBuf,
    },
}

impl Display for ExternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExternalEvent::Command { cmd, .. } => write!(f, "CommandEvent: {cmd}"),
            ExternalEvent::Shutdown { .. } => write!(f, "ShutdownEvent"),
            ExternalEvent::ConfigReload { path } => write!(f, "ConfigReloadEvent: {:?}", path),
        }
    }
}

pub fn command(cmd: Command, resp: oneshot::Sender<Response>) -> ExternalEvent {
    ExternalEvent::Command { cmd, resp }
}

pub fn shutdown(resp: oneshot::Sender<()>) -> ExternalEvent {
    ExternalEvent::Shutdown { resp }
}

pub fn config_reload(path: PathBuf) -> ExternalEvent {
    ExternalEvent::ConfigReload { path }
}

impl Debug for ExternalEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExternalEvent::Command { cmd, .. } => f.debug_struct("ExternalEvent::Command").field("cmd", cmd).finish(),
            ExternalEvent::Shutdown { .. } => f.debug_struct("ExternalEvent::Shutdown").finish(),
            ExternalEvent::ConfigReload { path } => f
                .debug_struct("ExternalEvent::ConfigReload")
                .field("path", path)
                .finish(),
        }
    }
}
