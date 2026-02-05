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
    pub bridge: ByteSize,
    pub ping: ByteSize,
    pub main: ByteSize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaxSurbUpstream {
    pub bridge: Bandwidth,
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
    ) -> Self {
        Self {
            sessions,
            ping_options,
            buffer_sizes,
            max_surb_upstream,
            timeouts,
            announced_peer_minimum_score,
        }
    }
}

impl Default for MaxSurbUpstream {
    fn default() -> Self {
        Self {
            bridge: Bandwidth::from_kbps(512),
            ping: Bandwidth::from_mbps(1),
            main: Bandwidth::from_mbps(16),
        }
    }
}

impl Default for BufferSizes {
    fn default() -> Self {
        Self {
            bridge: ByteSize::kb(32),
            ping: ByteSize::kb(32),
            // using maximum allowed session buffer size
            main: ByteSize::mb(10),
        }
    }
}
