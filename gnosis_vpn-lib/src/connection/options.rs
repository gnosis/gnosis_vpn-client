use std::time::Duration;

use bytesize::ByteSize;
use edgli::hopr_lib::{SessionCapabilities, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;

use crate::ping;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub timeouts: Timeouts,
    pub sessions: Sessions,
    pub ping_options: ping::PingOptions,
    pub buffer_sizes: BufferSizes,
    pub max_surb_upstream: MaxSurbUpstream,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sessions {
    pub bridge: SessionParameters,
    pub wg: SessionParameters,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeouts {
    pub ping_retries: Duration,
    pub http: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionParameters {
    pub target: SessionTarget,
    pub capabilities: SessionCapabilities,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BufferSizes {
    pub bridge: ByteSize,
    pub ping: ByteSize,
    pub main: ByteSize,
}

#[derive(Clone, Debug, PartialEq)]
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
        ping_options: ping::PingOptions,
        buffer_sizes: BufferSizes,
        max_surb_upstream: MaxSurbUpstream,
        timeouts: Timeouts,
    ) -> Self {
        Self {
            sessions,
            ping_options,
            buffer_sizes,
            max_surb_upstream,
            timeouts,
        }
    }
}

impl Default for MaxSurbUpstream {
    fn default() -> Self {
        Self {
            bridge: Bandwidth::ZERO,
            ping: Bandwidth::from_mbps(1),
            main: Bandwidth::from_mbps(16),
        }
    }
}

impl Default for BufferSizes {
    fn default() -> Self {
        Self {
            bridge: ByteSize::b(0),
            ping: ByteSize::kb(32),
            main: ByteSize::mb(2),
        }
    }
}
