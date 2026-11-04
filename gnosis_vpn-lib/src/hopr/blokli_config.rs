use edgli::BlockchainConnectorConfig;
use serde::{Deserialize, Serialize};

use std::time::Duration;

/// Wider than edgli's default: consumer links need several TLS round trips, and a loaded Blokli answers in seconds.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Only looked up while DNS is still open, so a slow answer is a broken resolver; the startup pin covers it.
const DEFAULT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlokliConfig {
    pub connection_sync_timeout: Duration,
    pub sync_tolerance: usize,
    /// Bound on one request to Blokli, applied to the [`edgli::BlokliEndpoint`] rather than to
    /// [`BlockchainConnectorConfig`] - see the `From` impl below.
    pub request_timeout: Duration,
    /// Bound on root's lookup of the Blokli host before the first killswitch arm.
    pub resolve_timeout: Duration,
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
            nonce_gap_tolerance: defaults.nonce_gap_tolerance,
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
            resolve_timeout: DEFAULT_RESOLVE_TIMEOUT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_request_timeout_is_thirty_seconds() {
        assert_eq!(BlokliConfig::default().request_timeout, Duration::from_secs(30));
    }

    /// The connector's own default is deliberately tighter than ours; if upstream ever
    /// widens it to match, this override can go away.
    #[test]
    fn default_request_timeout_is_wider_than_the_connector_default() {
        assert!(BlokliConfig::default().request_timeout > edgli::DEFAULT_REQUEST_TIMEOUT);
    }
}
