use serde::{Deserialize, Serialize};

use crate::command::{Command, Response};
use crate::config::Config;
use crate::hopr_params::HoprParams;

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerCommand {
    HoprParams { hopr_params: HoprParams },
    Config { config: Config },
    Shutdown,
    Command { cmd: Command },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerResponse {
    Response { resp: Box<Response> },
    Ack,
}
