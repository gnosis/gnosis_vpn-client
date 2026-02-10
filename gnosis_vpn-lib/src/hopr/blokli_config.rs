use edgli::BlockchainConnectorConfig;
use serde::{Deserialize, Serialize};

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub tx_confirm_timeout: Duration,
    pub connection_timeout: Duration,
    pub sync_tolerance: usize,
}

impl From<BlokliConfig> for BlockchainConnectorConfig {
    fn from(config: BlokliConfig) -> Self {
        BlockchainConnectorConfig {
            tx_confirm_timeout: config.tx_confirm_timeout,
            connection_timeout: config.connection_timeout,
            sync_tolerance: config.sync_tolerance,
        }
    }
}
