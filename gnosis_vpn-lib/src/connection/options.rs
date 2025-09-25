use std::ops::Range;
use std::time::Duration;

use crate::{ping, session};

#[derive(Clone, Debug)]
pub struct Options {
    pub(super) bridge: SessionParameters,
    pub(super) wg: SessionParameters,
    pub(super) ping_interval: Range<u8>,
    pub(super) ping_options: ping::PingOptions,
    pub(super) buffer_sizes: BufferSizes,
    pub(super) max_surb_upstream: MaxSurbUpstream,
    pub(super) ping_retries_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct SessionParameters {
    pub(super) target: session::Target,
    pub(super) capabilities: Vec<session::Capability>,
}

#[derive(Clone, Debug)]
pub struct BufferSizes {
    pub bridge: String,
    pub ping: String,
    pub main: String,
}

#[derive(Clone, Debug)]
pub struct MaxSurbUpstream {
    pub bridge: String,
    pub ping: String,
    pub main: String,
}

impl SessionParameters {
    pub fn new(target: session::Target, capabilities: Vec<session::Capability>) -> Self {
        Self { target, capabilities }
    }
}

impl BufferSizes {
    pub fn new(bridge: String, ping: String, main: String) -> Self {
        Self { bridge, ping, main }
    }
}

impl MaxSurbUpstream {
    pub fn new(bridge: String, ping: String, main: String) -> Self {
        Self { bridge, ping, main }
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
    ) -> Self {
        Self {
            bridge,
            wg,
            ping_interval,
            ping_options,
            buffer_sizes,
            max_surb_upstream,
            ping_retries_timeout,
        }
    }
}

impl Default for MaxSurbUpstream {
    fn default() -> Self {
        MaxSurbUpstream::new("0 Mb/s".to_string(), "1 Mb/s".to_string(), "16 Mb/s".to_string())
    }
}

impl Default for BufferSizes {
    fn default() -> Self {
        BufferSizes::new("0 B".to_string(), "32 kB".to_string(), "2 MB".to_string())
    }
}
