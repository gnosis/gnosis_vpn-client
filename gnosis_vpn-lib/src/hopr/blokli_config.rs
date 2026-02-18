use edgli::BlockchainConnectorConfig;
use serde::{Deserialize, Serialize};

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub connection_sync_timeout: Duration,
    pub sync_tolerance: usize,
}

impl From<BlokliConfig> for BlockchainConnectorConfig {
    fn from(config: BlokliConfig) -> Self {
        BlockchainConnectorConfig {
            connection_sync_timeout: config.connection_sync_timeout,
            sync_tolerance: config.sync_tolerance,
        }
    }
}
