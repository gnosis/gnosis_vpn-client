use edgli::BlockchainConnectorConfig;
use serde::{Deserialize, Serialize};

use std::time::Duration;

/// Timeout for a single Blokli request.
///
/// Wider than the connector's own default because this client runs on consumer links, where
/// the TLS handshake alone can take several round trips before the request goes out.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub connection_sync_timeout: Duration,
    pub sync_tolerance: usize,
    /// Bound on one request to Blokli, applied to the [`edgli::BlokliEndpoint`] rather than to
    /// [`BlockchainConnectorConfig`] - see the `From` impl below.
    pub request_timeout: Duration,
}

impl From<BlokliConfig> for BlockchainConnectorConfig {
    fn from(config: BlokliConfig) -> Self {
        let defaults = BlockchainConnectorConfig::default();
        // `request_timeout` is deliberately absent: the connector receives an already-built
        // Blokli client, so the timeout belongs to the endpoint that builds it.
        BlockchainConnectorConfig {
            connection_sync_timeout: config.connection_sync_timeout,
            sync_tolerance: config.sync_tolerance,
            tx_timeout_multiplier: defaults.tx_timeout_multiplier,
        }
    }
}

impl Default for BlokliConfig {
    fn default() -> Self {
        let def = BlockchainConnectorConfig::default();
        Self {
            connection_sync_timeout: def.connection_sync_timeout,
            // Edge client uses lower tolerance than full nodes (default 90%) because
            // its blokli index typically covers only a fraction of all on-chain accounts.
            sync_tolerance: 50,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_request_timeout_is_ten_seconds() {
        assert_eq!(BlokliConfig::default().request_timeout, Duration::from_secs(10));
    }

    /// The connector's own default is deliberately tighter than ours; if upstream ever
    /// widens it to match, this override can go away.
    #[test]
    fn default_request_timeout_is_wider_than_the_connector_default() {
        assert!(BlokliConfig::default().request_timeout > edgli::DEFAULT_REQUEST_TIMEOUT);
    }
}
