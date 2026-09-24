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

    /// Attempts *in addition to* the first for a deposit transfer. `pix-test` only; ignored by the
    /// Curvy pool, which allocates deposits out of a shielded float rather than transferring each.
    #[serde(default = "PixConfig::default_max_deposit_retries")]
    pub max_deposit_retries: usize,

    /// wxHOPR the Safe must still hold after a deposit; a deposit that would breach it is refused.
    /// `pix-test` only; ignored by the Curvy pool, whose float is shielded once rather than paid out
    /// of the Safe per deposit.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_min_safe_hopr_reserve")]
    pub min_safe_hopr_reserve: HoprBalance,
}

impl Default for PixConfig {
    fn default() -> Self {
        let strategy = edgli::strategy::PixEntryStrategy::default();
        let pool = edgli::strategy::PixEntryPool::default();
        Self {
            price_per_byte: strategy.price_per_byte,
            max_ssa_allocation: strategy.max_ssa_allocation,
            max_spend_per_window: strategy.max_spend_per_window,
            spend_window: strategy.spend_window,
            deposit_buffer_period: strategy.deposit_buffer_period,
            max_deposit_tracking_time: pool.max_deposit_tracking_time,
            max_deposit_retries: Self::default_max_deposit_retries(),
            min_safe_hopr_reserve: Self::default_min_safe_hopr_reserve(),
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

    #[cfg(feature = "pix-test")]
    fn default_max_deposit_retries() -> usize {
        edgli::strategy::PixEntryPool::default().max_deposit_retries
    }

    #[cfg(feature = "pix-test")]
    fn default_min_safe_hopr_reserve() -> HoprBalance {
        edgli::strategy::PixEntryPool::default().min_safe_hopr_reserve
    }

    // The Curvy pool has neither knob; the fields stay so that one config file parses under both builds.
    #[cfg(feature = "pix-curvy")]
    fn default_max_deposit_retries() -> usize {
        0
    }

    #[cfg(feature = "pix-curvy")]
    fn default_min_safe_hopr_reserve() -> HoprBalance {
        HoprBalance::zero()
    }

    /// The edgli form, for the pool this build selected.
    ///
    /// `blokli_url` and `state_home` are only read by the Curvy pool: it talks to Blokli itself
    /// (deployment discovery, note index, submissions) and keeps durable state that has to be found
    /// again on every start, so it goes next to the rest of the client's state rather than in the
    /// working directory. Its deployment knobs — relayer URL, shielding, submission, token id, note
    /// source, initial funding — are left at upstream's defaults and set through its
    /// `HOPRD_CURVY_*` environment overrides, as for a hoprd node.
    pub fn to_entry_config(
        &self,
        #[cfg_attr(feature = "pix-test", allow(unused_variables))] blokli_url: &url::Url,
        #[cfg_attr(feature = "pix-test", allow(unused_variables))] state_home: &std::path::Path,
    ) -> edgli::strategy::PixEntryConfig {
        #[cfg(feature = "pix-test")]
        let pool = edgli::strategy::PixEntryPool {
            max_deposit_tracking_time: self.max_deposit_tracking_time,
            max_deposit_retries: self.max_deposit_retries,
            min_safe_hopr_reserve: self.min_safe_hopr_reserve,
        };
        #[cfg(feature = "pix-curvy")]
        let pool = {
            if self.max_deposit_retries != Self::default_max_deposit_retries()
                || self.min_safe_hopr_reserve != Self::default_min_safe_hopr_reserve()
            {
                tracing::warn!(
                    "pix_strategy.max_deposit_retries / min_safe_hopr_reserve are set but ignored: the Curvy pool has no such knobs"
                );
            }
            edgli::strategy::PixEntryPool {
                blokli_url: blokli_url.clone(),
                max_deposit_tracking_time: self.max_deposit_tracking_time,
                state_path: Some(state_home.join("curvy-pix.redb")),
                ..Default::default()
            }
        };
        edgli::strategy::PixEntryConfig {
            strategy: edgli::strategy::PixEntryStrategy {
                price_per_byte: self.price_per_byte,
                max_ssa_allocation: self.max_ssa_allocation,
                max_spend_per_window: self.max_spend_per_window,
                spend_window: self.spend_window,
                deposit_buffer_period: self.deposit_buffer_period,
            },
            pool,
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
        assert_eq!(parsed.max_deposit_retries, 7);
        assert_eq!(parsed.price_per_byte, def.price_per_byte);
        assert_eq!(parsed.max_ssa_allocation, def.max_ssa_allocation);
        assert_eq!(parsed.max_spend_per_window, def.max_spend_per_window);
        assert_eq!(parsed.spend_window, def.spend_window);
        assert_eq!(parsed.deposit_buffer_period, def.deposit_buffer_period);
        assert_eq!(parsed.max_deposit_tracking_time, def.max_deposit_tracking_time);
        assert_eq!(parsed.min_safe_hopr_reserve, def.min_safe_hopr_reserve);
    }
}
