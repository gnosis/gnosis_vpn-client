use serde::{Deserialize, Serialize};

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub tx_confirm_timeout: Duration,
    pub connection_timeout: Duration,
    pub sync_tolerance: u8,
}
