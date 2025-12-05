//! This module indicates external events that will be forwarded into core application loop.

use crate::command::{Command, Response};
use tokio::sync::oneshot;

#[derive(Debug)]
pub enum Incoming {
    Command {
        cmd: Command,
        resp: oneshot::Sender<Response>,
    },
    Shutdown,
}
