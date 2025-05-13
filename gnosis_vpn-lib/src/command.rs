use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::log_output;
use crate::peer_id::PeerId;

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Status,
    Connect(PeerId),
    ConnectMeta((String, String)),
    Disconnect,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = log_output::serialize(self);
        write!(f, "{}", s)
    }
}

impl FromStr for Command {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}
