use std::path::PathBuf;
use std::time::Duration;

use edgli::hopr_lib::api::types::primitive::prelude::HoprBalance;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

/// Operator-tunable parameters for the PIX exit-incentivization strategy; unset fields fall back to upstream defaults.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PixConfig {
    /// wxHOPR charged per byte of the agreed per-SSA quota.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_price_per_byte")]
    pub price_per_byte: HoprBalance,

    /// Ceiling on a single SSA deposit; a computed deposit above this is refused outright.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_max_ssa_allocation")]
    pub max_ssa_allocation: HoprBalance,

    /// Aggregate wxHOPR the strategy will commit to deposits within any `spend_window`; zero disables the limit.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_max_spend_per_window")]
    pub max_spend_per_window: HoprBalance,

    /// Rolling window `max_spend_per_window` is measured over.
    #[serde(with = "humantime_serde", default = "PixConfig::default_spend_window")]
    pub spend_window: Duration,

    /// Debounce window before a batch of pending deposits is flushed.
    #[serde(with = "humantime_serde", default = "PixConfig::default_deposit_buffer_period")]
    pub deposit_buffer_period: Duration,

    /// How long the pool keeps polling a stealth address for a deposit to land.
    #[serde(with = "humantime_serde", default = "PixConfig::default_max_deposit_tracking_time")]
    pub max_deposit_tracking_time: Duration,

    /// Attempts *in addition to* the first for a deposit transfer; unset keeps the pool's default.
    /// `pix-test` pool only: edgli warns about and ignores it under the Curvy pool, which allocates
    /// deposits out of a shielded float rather than transferring each.
    #[serde(default)]
    pub max_deposit_retries: Option<usize>,

    /// wxHOPR the Safe must still hold after a deposit; a deposit that would breach it is refused.
    /// Unset keeps the pool's default. `pix-test` pool only: edgli warns about and ignores it under
    /// the Curvy pool, whose float is shielded once rather than paid out of the Safe per deposit.
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    pub min_safe_hopr_reserve: Option<HoprBalance>,
}

impl Default for PixConfig {
    fn default() -> Self {
        let strategy = edgli::strategy::PixEntryStrategy::default();
        Self {
            price_per_byte: strategy.price_per_byte,
            max_ssa_allocation: strategy.max_ssa_allocation,
            max_spend_per_window: strategy.max_spend_per_window,
            spend_window: strategy.spend_window,
            deposit_buffer_period: strategy.deposit_buffer_period,
            max_deposit_tracking_time: Self::default_max_deposit_tracking_time(),
            max_deposit_retries: None, // Applies to pix-test pool only.
            min_safe_hopr_reserve: None,
        }
    }
}

// Per-field defaults (container-level `#[serde(default)]` doesn't survive `serde_as`); each builds only the upstream struct it needs.
impl PixConfig {
    fn default_price_per_byte() -> HoprBalance {
        edgli::strategy::PixEntryStrategy::default().price_per_byte
    }

    fn default_max_ssa_allocation() -> HoprBalance {
        edgli::strategy::PixEntryStrategy::default().max_ssa_allocation
    }

    fn default_max_spend_per_window() -> HoprBalance {
        edgli::strategy::PixEntryStrategy::default().max_spend_per_window
    }

    fn default_spend_window() -> Duration {
        edgli::strategy::PixEntryStrategy::default().spend_window
    }

    fn default_deposit_buffer_period() -> Duration {
        edgli::strategy::PixEntryStrategy::default().deposit_buffer_period
    }

    fn default_max_deposit_tracking_time() -> Duration {
        edgli::strategy::PixEntryPool::default().max_deposit_tracking_time
    }

    /// The edgli form; which pool the knobs reach is edgli's call, deployment stays on `HOPRD_CURVY_*`.
    pub fn to_entry_config(&self, state_home: PathBuf) -> edgli::strategy::PixEntryConfig {
        edgli::strategy::PixEntryConfig {
            strategy: edgli::strategy::PixEntryStrategy {
                price_per_byte: self.price_per_byte,
                max_ssa_allocation: self.max_ssa_allocation,
                max_spend_per_window: self.max_spend_per_window,
                spend_window: self.spend_window,
                deposit_buffer_period: self.deposit_buffer_period,
            },
            pool: edgli::strategy::PixEntryPool::from_knobs(edgli::strategy::PixEntryPoolKnobs {
                max_deposit_tracking_time: Some(self.max_deposit_tracking_time),
                max_deposit_retries: self.max_deposit_retries,
                min_safe_hopr_reserve: self.min_safe_hopr_reserve,
            }),
            state_dir: Some(state_home),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_falls_back_to_pix_config_default() {
        let parsed: PixConfig = toml::from_str("max_deposit_retries = 7").expect("valid partial PixConfig");
        let def = PixConfig::default();
        assert_eq!(parsed.max_deposit_retries, Some(7));
        assert_eq!(parsed.price_per_byte, def.price_per_byte);
        assert_eq!(parsed.max_ssa_allocation, def.max_ssa_allocation);
        assert_eq!(parsed.max_spend_per_window, def.max_spend_per_window);
        assert_eq!(parsed.spend_window, def.spend_window);
        assert_eq!(parsed.deposit_buffer_period, def.deposit_buffer_period);
        assert_eq!(parsed.max_deposit_tracking_time, def.max_deposit_tracking_time);
        assert_eq!(parsed.min_safe_hopr_reserve, def.min_safe_hopr_reserve);
    }

    #[test]
    fn to_entry_config_keeps_pool_state_in_state_home() {
        let cfg = PixConfig::default().to_entry_config(PathBuf::from("/var/lib/gnosisvpn"));
        assert_eq!(cfg.state_dir, Some(PathBuf::from("/var/lib/gnosisvpn")));
    }
}
