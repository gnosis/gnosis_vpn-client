use edgli::BlockchainConnectorConfig;
use serde::{Deserialize, Serialize};

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub tx_confirm_timeout: Duration,
    pub connection_timeout: Duration,
    pub sync_tolerance: usize,
}

impl Into<BlockchainConnectorConfig> for BlokliConfig {
    fn into(self) -> BlockchainConnectorConfig {
        BlockchainConnectorConfig {
            tx_confirm_timeout: self.tx_confirm_timeout,
            connection_timeout: self.connection_timeout,
            sync_tolerance: self.sync_tolerance,
        }
    }
}
