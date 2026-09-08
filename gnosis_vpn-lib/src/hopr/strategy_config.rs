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

    /// Data volume a single channel should carry before it needs a top-up. `None` leaves edgli's default in place.
    pub channel_capacity: Option<ByteSize>,

    /// Data volume added to a channel's stake on top-up. `None` leaves edgli's default in place.
    pub topup_capacity: Option<ByteSize>,

    /// Channel balance (as data capacity) below which a top-up fires. `None` leaves edgli's default in place.
    pub lower_capacity_threshold: Option<ByteSize>,

    /// Minimum safe balance required before opening/funding any channel. `None` derives it from `channel_capacity`.
    pub min_safe_capacity_required: Option<ByteSize>,

    /// How each capacity field above converts to a wxHOPR stake. `None` leaves edgli's default in place.
    pub sizing_mode: Option<edgli::strategy::CapacitySizingMode>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        let def = edgli::strategy::IncentiveConfiguration::default();
        Self {
            min_open_channels: def.min_open_channels,
            target_open_channels: def.target_open_channels,
            channel_allowlist: def.channel_allowlist,
            channel_capacity: def.channel_capacity,
            topup_capacity: def.topup_capacity,
            lower_capacity_threshold: def.lower_capacity_threshold,
            min_safe_capacity_required: def.min_safe_capacity_required,
            sizing_mode: def.sizing_mode,
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
            topup_capacity: c.topup_capacity,
            lower_capacity_threshold: c.lower_capacity_threshold,
            min_safe_capacity_required: c.min_safe_capacity_required,
            sizing_mode: c.sizing_mode,
        }
    }
}
