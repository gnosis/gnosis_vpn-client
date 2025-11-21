//! This module holds indicates external events that will be sent from outside the core application loop into it.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, Response};
    use std::path::PathBuf;
    use tokio::sync::oneshot;

    #[test]
    fn command_constructor_wraps_command_and_response_channel() -> anyhow::Result<()> {
        let (tx, _rx) = oneshot::channel::<Response>();
        let evt = command(Command::Ping, tx);

        match evt {
            Event::Command { cmd, .. } => assert!(matches!(cmd, Command::Ping)),
            other => panic!("expected command event, got {:?}", other),
        }

        Ok(())
    }

    #[test]
    fn shutdown_constructor_wraps_shutdown_channel() -> anyhow::Result<()> {
        let (tx, _rx) = oneshot::channel();
        let evt = shutdown(tx);

        assert!(matches!(evt, Event::Shutdown { .. }));

        Ok(())
    }

    #[test]
    fn config_reload_constructor_preserves_path_payload() -> anyhow::Result<()> {
        let evt = config_reload(PathBuf::from("/tmp/config.toml"));

        match evt {
            Event::ConfigReload { path } => assert_eq!(path, PathBuf::from("/tmp/config.toml")),
            other => panic!("expected config reload, got {:?}", other),
        }
        Ok(())
    }
}
