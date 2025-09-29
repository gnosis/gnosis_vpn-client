use std::ops::Range;
use std::time::Duration;

use bytesize::ByteSize;
use edgli::hopr_lib::{SessionCapabilities, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;

use crate::ping;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub(super) bridge: SessionParameters,
    pub(super) wg: SessionParameters,
    pub(super) ping_interval: Range<u8>,
    pub(super) ping_options: ping::PingOptions,
    pub(super) buffer_sizes: BufferSizes,
    pub(super) max_surb_upstream: MaxSurbUpstream,
    pub(super) ping_retries_timeout: Duration,
    pub(super) http_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionParameters {
    pub(super) target: SessionTarget,
    pub(super) capabilities: SessionCapabilities,
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
        bridge: SessionParameters,
        wg: SessionParameters,
        ping_interval: Range<u8>,
        ping_options: ping::PingOptions,
        buffer_sizes: BufferSizes,
        max_surb_upstream: MaxSurbUpstream,
        ping_retries_timeout: Duration,
        http_timeout: Duration,
    ) -> Self {
        Self {
            bridge,
            wg,
            ping_interval,
            ping_options,
            buffer_sizes,
            max_surb_upstream,
            ping_retries_timeout,
            http_timeout,
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
