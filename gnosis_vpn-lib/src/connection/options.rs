pub const DEFAULT_PATH_PLANNER_MIN_ACK_RATE: f64 = 0.1;

use bytesize::ByteSize;
use edgli::PathPlannerConfig;
use edgli::hopr_lib::exports::transport::{SessionCapabilities, SessionTarget, SurbBalancerConfig};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use std::time::Duration;

use crate::ping;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub timeouts: Timeouts,
    pub sessions: Sessions,
    pub ping_options: ping::Options,
    pub surb_balancing: SurbBalancing,
    pub pix: PixOptions,
    pub health_check_intervals: HealthCheckIntervals,
    pub lan_lockdown: bool,
    /// Whether announced peer addresses that are private/local IPs (loopback, RFC1918,
    /// link-local) are probed/trusted rather than filtered out. Defaults to false since a
    /// real exit node announcing a private IP is almost certainly misconfigured or spoofed;
    /// enable for test/dev setups where peers legitimately announce local addresses.
    pub probe_local_addresses: bool,
    /// Minimum acknowledgement rate [0.0, 1.0] a path must sustain to be considered by
    /// the latency path planner. Paths below this threshold are skipped.
    pub path_planner_min_ack_rate: f64,
    /// Overrides layered on the edge-client latency preset; empty leaves the preset unchanged.
    pub path_planner: PathPlannerOptions,
}

/// Optional overrides mirroring [`PathPlannerConfig`]: only set fields override the preset; `min_ack_rate` stays flat.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct PathPlannerOptions {
    /// Maximum number of entries in the path cache.
    pub max_cache_capacity: Option<u64>,
    /// Time-to-live for a cached path list (humantime, e.g. "10s").
    #[serde(default, with = "humantime_serde::option")]
    pub cache_ttl: Option<Duration>,
    /// Period between proactive background cache-refresh sweeps (humantime).
    #[serde(default, with = "humantime_serde::option")]
    pub refresh_period: Option<Duration>,
    /// Maximum number of candidate paths the selector may return per query.
    pub max_cached_paths: Option<usize>,
    /// Penalty multiplier for edges lacking probe-based quality observations. Must be in [0.0, 1.0].
    #[serde(default, deserialize_with = "validate_unit_interval_opt")]
    pub edge_penalty: Option<f64>,
    /// Cap on retained candidate paths, slowest dropped first; 0 keeps every path.
    pub min_paths_anonymity_floor: Option<usize>,
    /// Total path latency at which the latency factor equals 0.5 (humantime).
    #[serde(default, with = "humantime_serde::option")]
    pub latency_halflife: Option<Duration>,
    /// Capacity saturation point in single-hop tickets; `u64` because TOML integers are, widened on apply.
    pub capacity_reference: Option<u64>,
    /// Exponent applied to return-path weights before sampling. Must be in (0.0, 1.0].
    #[serde(default, deserialize_with = "validate_weight_temper_opt")]
    pub return_path_weight_temper: Option<f64>,
    /// Fraction of return-path draws made uniformly at random. Must be in [0.0, 1.0].
    #[serde(default, deserialize_with = "validate_unit_interval_opt")]
    pub return_path_exploration: Option<f64>,
    /// Upper bound on a plausible loopback probe round-trip time (humantime).
    #[serde(default, with = "humantime_serde::option")]
    pub max_plausible_loopback_rtt: Option<Duration>,
}

impl PathPlannerOptions {
    /// Apply the set overrides onto `cfg`, leaving unset fields untouched.
    pub fn apply(&self, cfg: &mut PathPlannerConfig) {
        if let Some(v) = self.max_cache_capacity {
            cfg.max_cache_capacity = v;
        }
        if let Some(v) = self.cache_ttl {
            cfg.cache_ttl = v;
        }
        if let Some(v) = self.refresh_period {
            cfg.refresh_period = v;
        }
        if let Some(v) = self.max_cached_paths {
            cfg.max_cached_paths = v;
        }
        if let Some(v) = self.edge_penalty {
            cfg.edge_penalty = v;
        }
        if let Some(v) = self.min_paths_anonymity_floor {
            cfg.min_paths_anonymity_floor = v;
        }
        if let Some(v) = self.latency_halflife {
            cfg.latency_halflife = v;
        }
        if let Some(v) = self.capacity_reference {
            cfg.capacity_reference = u128::from(v);
        }
        if let Some(v) = self.return_path_weight_temper {
            cfg.return_path_weight_temper = v;
        }
        if let Some(v) = self.return_path_exploration {
            cfg.return_path_exploration = v;
        }
        if let Some(v) = self.max_plausible_loopback_rtt {
            cfg.max_plausible_loopback_rtt = v;
        }
    }
}

fn validate_unit_interval_opt<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<f64>::deserialize(deserializer)? {
        Some(v) if !(0.0..=1.0).contains(&v) => Err(serde::de::Error::custom("value must be in the range [0.0, 1.0]")),
        other => Ok(other),
    }
}

fn validate_weight_temper_opt<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<f64>::deserialize(deserializer)? {
        Some(v) if !(v > 0.0 && v <= 1.0) => Err(serde::de::Error::custom(
            "return_path_weight_temper must be in the range (0.0, 1.0]",
        )),
        other => Ok(other),
    }
}

/// Controls how often each tier of health check runs.
/// Ping runs every cycle. Health and version piggyback every Nth cycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthCheckIntervals {
    pub ping: Duration,
    /// Run exit health check every Nth ping cycle.
    pub health_every_n_pings: u32,
    /// Run version check every Nth ping cycle.
    pub version_every_n_pings: u32,
    /// Interval between ICMP tunnel ping probes when connected.
    pub tunnel_ping: Duration,
    /// Consecutive tunnel ping failures before triggering reconnect.
    pub tunnel_ping_max_failures: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub bridge: SessionParameters,
    pub wg: SessionParameters,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeouts {
    pub http: Duration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionParameters {
    pub target: SessionTarget,
    pub capabilities: SessionCapabilities,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSurbOptions {
    pub enabled: bool,
    pub buffer: ByteSize,
    pub max_surb_upstream: Bandwidth,
    /// When the balancer is inactive, send only 1 SURB per HTTP request even if 2 would fit.
    pub always_max_out_surbs: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurbBalancing {
    pub ping: SessionSurbOptions,
    pub main: SessionSurbOptions,
    pub bridge: SessionSurbOptions,
    pub health_check: SessionSurbOptions,
    pub ramp: SurbRampOptions,
}

/// Pacing of the post-connect ping->main SURB setpoint ramp (see `Up::advance_surb_ramp`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurbRampOptions {
    /// How often the setpoint is nudged toward the main tier.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// How long the full ping->main ramp takes to converge.
    #[serde(with = "humantime_serde")]
    pub duration: Duration,
}

impl Default for SurbRampOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            duration: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionPixOptions {
    pub enabled: bool,
}

/// Per-session-kind PIX enablement; `ping_main` is one toggle since ping and main share one session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PixOptions {
    pub ping_main: SessionPixOptions,
    pub bridge: SessionPixOptions,
    pub health_check: SessionPixOptions,
}

impl SessionParameters {
    pub fn new(target: SessionTarget, capabilities: SessionCapabilities) -> Self {
        Self { target, capabilities }
    }
}

impl SessionSurbOptions {
    pub fn new(enabled: bool, buffer: ByteSize, max_surb_upstream: Bandwidth) -> Self {
        Self {
            enabled,
            buffer,
            max_surb_upstream,
            always_max_out_surbs: enabled,
        }
    }
}

impl Default for HealthCheckIntervals {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(15),
            health_every_n_pings: 4,
            version_every_n_pings: 20,
            tunnel_ping: Duration::from_secs(10),
            tunnel_ping_max_failures: 3,
        }
    }
}

impl Default for SurbBalancing {
    fn default() -> Self {
        Self {
            ping: SessionSurbOptions::new(true, ByteSize::mb(1), Bandwidth::from_kbps(512)),
            // maximum allowed buffer size is 10 MB
            main: SessionSurbOptions::new(true, ByteSize::mb(10), Bandwidth::from_mbps(16)),
            bridge: SessionSurbOptions::new(false, ByteSize::kb(16), Bandwidth::from_kbps(128)),
            health_check: SessionSurbOptions::new(false, ByteSize::kb(16), Bandwidth::from_kbps(128)),
            ramp: SurbRampOptions::default(),
        }
    }
}

impl Default for PixOptions {
    fn default() -> Self {
        Self {
            ping_main: SessionPixOptions { enabled: true },
            bridge: SessionPixOptions { enabled: false },
            health_check: SessionPixOptions { enabled: false },
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum SurbConfigError {
    #[error("Response buffer byte size too small")]
    ResponseBufferTooSmall,
    #[error("Max SURB upstream bandwidth cannot be zero")]
    MaxSurbUpstreamCannotBeZero,
    #[error("Max SURB upstream bandwidth is too large to represent as a u64 SURB/s rate")]
    MaxSurbsPerSecOverflow,
}

#[derive(Debug)]
pub(crate) struct SurbParams {
    pub(crate) management: Option<SurbBalancerConfig>,
    pub(crate) always_max_out_surbs: bool,
}

pub(crate) fn surb_config_for(opts: &SessionSurbOptions) -> Result<SurbParams, SurbConfigError> {
    let management = if opts.enabled {
        to_surb_balancer_config(opts.buffer, opts.max_surb_upstream).map(Some)?
    } else {
        None
    };
    Ok(SurbParams {
        management,
        always_max_out_surbs: opts.always_max_out_surbs,
    })
}

pub(crate) fn to_surb_balancer_config(
    response_buffer: ByteSize,
    max_surb_upstream: Bandwidth,
) -> Result<SurbBalancerConfig, SurbConfigError> {
    if response_buffer.as_u64() < 2 * edgli::hopr_lib::exports::transport::SESSION_MTU as u64 {
        return Err(SurbConfigError::ResponseBufferTooSmall);
    }
    if max_surb_upstream.is_zero() {
        return Err(SurbConfigError::MaxSurbUpstreamCannotBeZero);
    }
    let max_surbs_per_sec_u128 =
        max_surb_upstream.as_bps() / (8 * edgli::hopr_lib::exports::transport::SURB_SIZE as u128);
    let max_surbs_per_sec =
        u64::try_from(max_surbs_per_sec_u128).map_err(|_| SurbConfigError::MaxSurbsPerSecOverflow)?;
    let config = SurbBalancerConfig {
        target_surb_buffer_size: response_buffer.as_u64() / edgli::hopr_lib::exports::transport::SESSION_MTU as u64,
        max_surbs_per_sec,
        sustain_on_return_path_loss: true, // keep producing SURBs when return-path loss hides consumed replies (GNO-713)
        ..Default::default()
    };
    Ok(config)
}
