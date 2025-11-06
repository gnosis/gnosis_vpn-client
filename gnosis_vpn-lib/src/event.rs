use std::fmt::{self, Debug, Display};
use std::path::PathBuf;
use tokio::sync::oneshot;

use crate::command::{Command, Response};

pub enum Event {
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

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Command { cmd, .. } => write!(f, "CommandEvent: {cmd}"),
            Event::Shutdown { .. } => write!(f, "ShutdownEvent"),
            Event::ConfigReload { path } => write!(f, "ConfigReloadEvent: {:?}", path),
        }
    }
}

pub fn command(cmd: Command, resp: oneshot::Sender<Response>) -> Event {
    Event::Command { cmd, resp }
}

pub fn shutdown(resp: oneshot::Sender<()>) -> Event {
    Event::Shutdown { resp }
}

pub fn config_reload(path: PathBuf) -> Event {
    Event::ConfigReload { path }
}

impl Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Command { cmd, .. } => f.debug_struct("Event::Command").field("cmd", cmd).finish(),
            Event::Shutdown { .. } => f.debug_struct("Event::Shutdown").finish(),
            Event::ConfigReload { path } => f.debug_struct("Event::ConfigReload").field("path", path).finish(),
        }
    }
}
