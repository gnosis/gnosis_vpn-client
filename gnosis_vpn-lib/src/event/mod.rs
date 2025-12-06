//! This module holds event definitions for inter process communication between root <-> worker and worker <-> core.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::command::{Command, Response};
use crate::config::Config;
use crate::hopr_params::HoprParams;

#[derive(Debug)]
pub enum IncomingCore {
    Command {
        cmd: Command,
        resp: oneshot::Sender<Response>,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum OutgoingCore {
    WgUp(String),
    WgDown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IncomingWorker {
    HoprParams { hopr_params: HoprParams },
    Config { config: Config },
    Shutdown,
    Command { cmd: Command },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum OutoingWorker {
    Response { resp: Box<Response> },
    Ack,
}
