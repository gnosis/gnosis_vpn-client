use bytesize::ByteSize;
use edgli::hopr_lib::exports::transport::{SessionCapabilities, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Serialize};

use std::time::Duration;

use crate::ping;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub timeouts: Timeouts,
    pub sessions: Sessions,
    pub ping_options: ping::Options,
    pub surb_balancing: SurbBalancing,
    pub health_check_intervals: HealthCheckIntervals,
    pub lan_lockdown: bool,
    /// How long to keep a closed session's pseudonym cached for potential reuse on reconnect.
    /// Exit nodes retain session SURBs for ~30s, so reconnecting within this window
    /// avoids a cold-start SURB exchange. Currently set to 1s (effectively disabled)
    /// until hopr-lib supports PIX.
    pub session_pseudonym_ttl: Duration,
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
