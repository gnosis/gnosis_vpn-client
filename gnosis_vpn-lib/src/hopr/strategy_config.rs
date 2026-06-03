use serde::{Deserialize, Serialize};

/// Sizing and topology parameters for the channel lifecycle strategy reactor.
///
/// Maps 1-to-1 to [`edgli::strategy::IncentiveConfiguration`]; carried in the gnosis_vpn
/// config layer so operators can tune channel funding from the TOML config file.
///
/// All fields default to the same values as [`edgli::strategy::IncentiveConfiguration`].
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
}

impl Default for StrategyConfig {
    fn default() -> Self {
        let def = edgli::strategy::IncentiveConfiguration::default();
        Self {
            desired_message_count: def.desired_message_count,
            min_open_channels: def.min_open_channels,
            target_open_channels: def.target_open_channels,
        }
    }
}

impl From<StrategyConfig> for edgli::strategy::IncentiveConfiguration {
    fn from(c: StrategyConfig) -> Self {
        Self {
            desired_message_count: c.desired_message_count,
            min_open_channels: c.min_open_channels,
            target_open_channels: c.target_open_channels,
            // Derived at runtime from destination routing modes in core, not a TOML knob.
            channel_allowlist: None,
        }
    }
}
