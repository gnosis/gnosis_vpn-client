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
    pub price_per_byte: HoprBalance,

    /// Ceiling on a single SSA deposit; a computed deposit above this is refused outright.
    #[serde_as(as = "DisplayFromStr")]
    pub max_ssa_allocation: HoprBalance,

    /// Debounce window before a batch of pending deposits is flushed.
    #[serde(with = "humantime_serde")]
    pub deposit_buffer_period: Duration,

    /// How long the pool keeps polling a stealth address for a deposit to land.
    #[serde(with = "humantime_serde")]
    pub max_deposit_tracking_time: Duration,

    /// Attempts *in addition to* the first for a deposit transfer.
    pub max_deposit_retries: usize,
}

impl Default for PixConfig {
    fn default() -> Self {
        let strategy = edgli::strategy::PixEntryStrategy::default();
        let pool = edgli::strategy::PixEntryPool::default();
        Self {
            price_per_byte: strategy.price_per_byte,
            max_ssa_allocation: strategy.max_ssa_allocation,
            deposit_buffer_period: strategy.deposit_buffer_period,
            max_deposit_tracking_time: pool.max_deposit_tracking_time,
            max_deposit_retries: pool.max_deposit_retries,
        }
    }
}

impl From<PixConfig> for edgli::strategy::PixEntryConfig {
    fn from(c: PixConfig) -> Self {
        Self {
            strategy: edgli::strategy::PixEntryStrategy {
                price_per_byte: c.price_per_byte,
                max_ssa_allocation: c.max_ssa_allocation,
                deposit_buffer_period: c.deposit_buffer_period,
            },
            pool: edgli::strategy::PixEntryPool {
                max_deposit_tracking_time: c.max_deposit_tracking_time,
                max_deposit_retries: c.max_deposit_retries,
            },
        }
    }
}
