use bytesize::ByteSize;
use edgli::hopr_lib::{SessionCapabilities, SessionTarget};
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
    pub announced_peer_minimum_score: f64,
    pub health_check_intervals: HealthCheckIntervals,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub bridge: SessionParameters,
    pub health: SessionParameters,
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
    pub bridge: ByteSize,
    pub health: ByteSize,
    pub ping: ByteSize,
    pub main: ByteSize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaxSurbUpstream {
    pub bridge: Bandwidth,
    pub health: Bandwidth,
    pub ping: Bandwidth,
    pub main: Bandwidth,
}

impl SessionParameters {
    pub fn new(target: SessionTarget, capabilities: SessionCapabilities) -> Self {
        Self { target, capabilities }
    }
}

impl Options {
    pub fn new(
        sessions: Sessions,
        ping_options: ping::Options,
        buffer_sizes: BufferSizes,
        max_surb_upstream: MaxSurbUpstream,
        timeouts: Timeouts,
        announced_peer_minimum_score: f64,
        health_check_intervals: HealthCheckIntervals,
    ) -> Self {
        Self {
            sessions,
            ping_options,
            buffer_sizes,
            max_surb_upstream,
            timeouts,
            announced_peer_minimum_score,
            health_check_intervals,
        }
    }
}

impl Default for HealthCheckIntervals {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(30),
            health_every_n_pings: 2,
            version_every_n_pings: 10,
        }
    }
}

impl Default for MaxSurbUpstream {
    fn default() -> Self {
        Self {
            bridge: Bandwidth::from_kbps(512),
            health: Bandwidth::from_kbps(256),
            ping: Bandwidth::from_kbps(512),
            main: Bandwidth::from_mbps(16),
        }
    }
}

impl Default for BufferSizes {
    fn default() -> Self {
        Self {
            bridge: ByteSize::kb(32),
            health: ByteSize::kb(16),
            ping: ByteSize::mb(1),
            // maximum allowed buffer size is 10 MB
            main: ByteSize::mb(10),
        }
    }
}
