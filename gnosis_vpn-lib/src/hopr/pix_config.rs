use std::path::PathBuf;
use std::time::Duration;

use edgli::hopr_lib::api::types::primitive::prelude::HoprBalance;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use url::Url;

/// Where the installer puts the Curvy proving files; only a v7 config can point elsewhere.
#[cfg(target_os = "linux")]
pub const DEFAULT_CURVY_ZK_DIR: &str = "/usr/share/gnosisvpn/curvy-zk";
#[cfg(target_os = "macos")]
pub const DEFAULT_CURVY_ZK_DIR: &str = "/usr/local/share/gnosisvpn/curvy-zk";

/// curvy-witnesscalc reads the proving files' directory from this variable only.
pub const CURVY_ZK_DIR_ENV: &str = "CURVY_ZK_KEYS_DIR";

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
    /// deposits out of a shielded pool rather than transferring each.
    #[serde(default)]
    pub max_deposit_retries: Option<usize>,

    /// wxHOPR the Safe must still hold after a deposit; a deposit that would breach it is refused.
    /// Unset keeps the pool's default. `pix-test` pool only: edgli warns about and ignores it under
    /// the Curvy pool, whose funds are shielded once rather than paid out of the Safe per deposit.
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    pub min_safe_hopr_reserve: Option<HoprBalance>,

    /// Curvy relayer that submits deposit proofs; clients hold no Curvy operator key, so they always go through it.
    /// Curvy pool only; the `pix-test` pool has no relayer.
    #[serde(default = "PixConfig::default_relayer_url")]
    pub relayer_url: Url,

    /// Curvy PIX pool balance below which the pool tops itself up from the Safe ahead of a deposit flush.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_funding_min_available")]
    pub funding_min_available: HoprBalance,

    /// wxHOPR one Curvy top-up shields from the Safe, gross of the shield fee.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "PixConfig::default_funding_top_up")]
    pub funding_top_up: HoprBalance,

    /// Top up once at startup; off because the PIX pool is first funded manually (`fundpixpool`).
    #[serde(default)]
    pub fund_at_startup: bool,

    /// Lag budget of one Curvy top-up; unset keeps the pool's default.
    #[serde(default, with = "humantime_serde::option")]
    pub max_funding_time: Option<Duration>,

    /// Wait for an unconfirmed Curvy shield before resending it; unset keeps the pool's default.
    #[serde(default, with = "humantime_serde::option")]
    pub direct_shield_settle_grace: Option<Duration>,
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
            relayer_url: Self::default_relayer_url(),
            funding_min_available: Self::default_funding_min_available(),
            funding_top_up: Self::default_funding_top_up(),
            fund_at_startup: false,
            max_funding_time: None,
            direct_shield_settle_grace: None,
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

    /// The relayer the exits submit through as well.
    pub(crate) fn default_relayer_url() -> Url {
        Url::parse("https://api.curvy.box").expect("default Curvy relayer URL is valid")
    }

    fn default_max_deposit_tracking_time() -> Duration {
        edgli::strategy::PixEntryPool::default().max_deposit_tracking_time
    }

    /// About three deposits of 0.66 wxHOPR, well under half of `funding_top_up` as upstream asks.
    fn default_funding_min_available() -> HoprBalance {
        "2 wxHOPR".parse().expect("default funding floor is valid")
    }

    /// The GNO-1151 onboarding PIX pool funding.
    fn default_funding_top_up() -> HoprBalance {
        "35.35 wxHOPR".parse().expect("default funding top-up is valid")
    }

    /// What one top-up shields from the Safe; the `pix-test` pool shields nothing.
    pub fn pool_top_up(&self) -> HoprBalance {
        #[cfg(feature = "pix-curvy")]
        return self.funding_top_up;
        #[cfg(not(feature = "pix-curvy"))]
        HoprBalance::zero()
    }

    /// The edgli form; which pool the knobs reach is edgli's call, deployment stays on `HOPRD_CURVY_*`.
    pub fn to_entry_config(
        &self,
        state_dir: PathBuf,
        state_password: edgli::strategy::PixRecoveryPassword,
    ) -> edgli::strategy::PixEntryConfig {
        let pool = edgli::strategy::PixEntryPool::from_knobs(edgli::strategy::PixEntryPoolKnobs {
            max_deposit_tracking_time: Some(self.max_deposit_tracking_time),
            max_deposit_retries: self.max_deposit_retries,
            min_safe_hopr_reserve: self.min_safe_hopr_reserve,
        });
        // edgli defaults to `Operator`, which needs a Curvy operator key clients never hold.
        #[cfg(feature = "pix-curvy")]
        let pool = edgli::strategy::PixEntryPool {
            submission: edgli::strategy::PixCurvySubmission::Relayer(self.relayer_url.clone()),
            funding: Some(edgli::strategy::PixFundingPolicy {
                min_available: self.funding_min_available,
                top_up: self.funding_top_up,
                fund_at_startup: self.fund_at_startup,
            }),
            max_funding_time: self.max_funding_time.unwrap_or(pool.max_funding_time),
            direct_shield_settle_grace: self
                .direct_shield_settle_grace
                .unwrap_or(pool.direct_shield_settle_grace),
            ..pool
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
            state_dir: Some(state_dir),
            state_password,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_password() -> edgli::strategy::PixRecoveryPassword {
        "test password".to_owned().into()
    }

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
        assert_eq!(parsed.relayer_url, def.relayer_url);
        assert_eq!(parsed.funding_min_available, def.funding_min_available);
        assert_eq!(parsed.funding_top_up, def.funding_top_up);
        assert_eq!(parsed.fund_at_startup, def.fund_at_startup);
    }

    #[test]
    fn to_entry_config_keeps_pool_state_in_the_network_pix_dir() {
        let pix_dir = PathBuf::from("/home/alice/.local/share/gnosisvpn/networks/jura-dev/pix");
        let cfg = PixConfig::default().to_entry_config(pix_dir.clone(), test_password());
        assert_eq!(cfg.state_dir, Some(pix_dir));
        assert_eq!(cfg.state_password, test_password());
    }

    #[cfg(feature = "pix-curvy")]
    #[test]
    fn to_entry_config_submits_through_curvy_relayer() {
        let cfg = PixConfig::default().to_entry_config(
            PathBuf::from("/home/alice/.local/share/gnosisvpn/networks/jura-dev/pix"),
            test_password(),
        );
        assert_eq!(
            cfg.pool.submission,
            edgli::strategy::PixCurvySubmission::Relayer(PixConfig::default_relayer_url())
        );
    }

    #[cfg(feature = "pix-curvy")]
    #[test]
    fn to_entry_config_always_carries_a_funding_policy() {
        let pix_dir = PathBuf::from("/home/alice/.local/share/gnosisvpn/networks/jura-dev/pix");
        let unset = PixConfig::default().to_entry_config(pix_dir.clone(), test_password());
        assert_eq!(
            unset.pool.funding,
            Some(edgli::strategy::PixFundingPolicy {
                min_available: PixConfig::default_funding_min_available(),
                top_up: PixConfig::default_funding_top_up(),
                fund_at_startup: false,
            })
        );
        assert_eq!(
            unset.pool.max_funding_time,
            edgli::strategy::PixEntryPool::default().max_funding_time
        );

        let set = PixConfig {
            funding_min_available: "1 wxHOPR".parse().expect("valid balance"),
            funding_top_up: "10 wxHOPR".parse().expect("valid balance"),
            fund_at_startup: true,
            max_funding_time: Some(Duration::from_secs(600)),
            direct_shield_settle_grace: Some(Duration::from_secs(60)),
            ..PixConfig::default()
        }
        .to_entry_config(pix_dir, test_password());
        assert_eq!(
            set.pool.funding,
            Some(edgli::strategy::PixFundingPolicy {
                min_available: "1 wxHOPR".parse().expect("valid balance"),
                top_up: "10 wxHOPR".parse().expect("valid balance"),
                fund_at_startup: true,
            })
        );
        assert_eq!(set.pool.max_funding_time, Duration::from_secs(600));
        assert_eq!(set.pool.direct_shield_settle_grace, Duration::from_secs(60));
    }
}
