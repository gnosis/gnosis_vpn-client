use std::fmt::{self, Display};
use std::path::PathBuf;
use tokio::sync::oneshot;

use gnosis_vpn_lib::command::{Command, Response};

#[derive(Debug)]
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
