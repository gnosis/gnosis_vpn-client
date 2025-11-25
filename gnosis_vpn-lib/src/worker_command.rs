use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::config::Config;
use crate::hopr_params::HoprParams;

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerCommand {
    HoprParams { hopr_params: HoprParams },
    Config { config: Config },
    Shutdown,
    Command { cmd: Command },
}
