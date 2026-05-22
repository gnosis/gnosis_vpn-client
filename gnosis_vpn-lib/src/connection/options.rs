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
    pub buffer_sizes: BufferSizes,
    pub max_surb_upstream: MaxSurbUpstream,
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
pub struct BufferSizes {
    pub ping: ByteSize,
    pub main: ByteSize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaxSurbUpstream {
    pub ping: Bandwidth,
    pub main: Bandwidth,
}

impl SessionParameters {
    pub fn new(target: SessionTarget, capabilities: SessionCapabilities) -> Self {
        Self { target, capabilities }
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

impl Default for MaxSurbUpstream {
    fn default() -> Self {
        Self {
            ping: Bandwidth::from_kbps(512),
            main: Bandwidth::from_mbps(16),
        }
    }
}

impl Default for BufferSizes {
    fn default() -> Self {
        Self {
            ping: ByteSize::mb(1),
            // maximum allowed buffer size is 10 MB
            // lowered to 5 MB as a compromise: the ping session currently inherits this value
            // due to a bug workaround (see TODO in connection/up/runner.rs)
            main: ByteSize::mb(5),
        }
    }
}
