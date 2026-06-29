use std::collections::HashSet;

use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};

/// Operator-tunable parameters for the channel lifecycle strategy reactor.
///
/// Exposes the subset of [`edgli::strategy::IncentiveConfiguration`] that operators
/// can tune via the TOML config file. Fields not listed here fall back to upstream
/// defaults (see [`edgli::strategy::IncentiveConfiguration::default`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Expected number of mixnet messages per channel before exhaustion.
    /// The initial stake is sized as the expected drain:
    /// `desired_message_count × win_prob × ticket_price`.
    pub desired_message_count: u64,

    /// Minimum number of open outgoing channels to maintain.
    pub min_open_channels: usize,

    /// Target number of open outgoing channels.
    pub target_open_channels: usize,

    /// When `Some`, channels are opened exclusively to these peers; `None` uses quality-score selection.
    pub channel_allowlist: Option<HashSet<Address>>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        let def = edgli::strategy::IncentiveConfiguration::default();
        Self {
            desired_message_count: def.desired_message_count,
            min_open_channels: def.min_open_channels,
            target_open_channels: def.target_open_channels,
            channel_allowlist: def.channel_allowlist,
        }
    }
}

impl From<StrategyConfig> for edgli::strategy::IncentiveConfiguration {
    fn from(c: StrategyConfig) -> Self {
        Self {
            desired_message_count: c.desired_message_count,
            min_open_channels: c.min_open_channels,
            target_open_channels: c.target_open_channels,
            channel_allowlist: c.channel_allowlist,
        }
    }
}
