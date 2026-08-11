use std::collections::HashSet;

use bytesize::ByteSize;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};

/// Operator-tunable parameters for the channel lifecycle strategy reactor.
///
/// Exposes the subset of [`edgli::strategy::IncentiveConfiguration`] that operators
/// can tune via the TOML config file. Fields not listed here fall back to upstream
/// defaults (see [`edgli::strategy::IncentiveConfiguration::default`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Minimum number of open outgoing channels to maintain.
    pub min_open_channels: usize,

    /// Target number of open outgoing channels.
    pub target_open_channels: usize,

    /// When `Some`, channels are opened exclusively to these peers; `None` uses quality-score selection.
    pub channel_allowlist: Option<HashSet<Address>>,

    /// Data volume a single channel should carry before it needs a top-up.
    ///
    /// The only sizing input we supply; edgli derives the capacities, sizing mode and
    /// stakes from it. `None` leaves edgli's default in place, and values below the
    /// one-winning-ticket floor have no effect.
    pub channel_capacity: Option<ByteSize>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        let def = edgli::strategy::IncentiveConfiguration::default();
        Self {
            min_open_channels: def.min_open_channels,
            target_open_channels: def.target_open_channels,
            channel_allowlist: def.channel_allowlist,
            channel_capacity: def.channel_capacity,
        }
    }
}

impl From<StrategyConfig> for edgli::strategy::IncentiveConfiguration {
    fn from(c: StrategyConfig) -> Self {
        Self {
            min_open_channels: c.min_open_channels,
            target_open_channels: c.target_open_channels,
            channel_allowlist: c.channel_allowlist,
            channel_capacity: c.channel_capacity,
        }
    }
}
