pub const DEFAULT_PATH_PLANNER_MIN_ACK_RATE: f64 = 0.1;

use bytesize::ByteSize;
use edgli::hopr_lib::exports::transport::{SessionCapabilities, SessionTarget, SurbBalancerConfig};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::time::Duration;

use crate::ping;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub timeouts: Timeouts,
    pub sessions: Sessions,
    pub ping_options: ping::Options,
    pub surb_balancing: SurbBalancing,
    pub pix: PixBalancing,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionPixOptions {
    pub enabled: bool,
}

/// Per-session-kind PIX enablement; `ping_main` is one toggle since ping and main share one session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PixBalancing {
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
        }
    }
}

impl Default for PixBalancing {
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
