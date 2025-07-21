use std::ops::Range;
use std::time::Duration;

use crate::{monitor, session};

#[derive(Clone, Debug)]
pub struct Options {
    pub(super) bridge: SessionParameters,
    pub(super) wg: SessionParameters,
    pub(super) ping_interval: Range<u8>,
    pub(super) ping_options: monitor::PingOptions,
    pub(super) buffer_sizes: BufferSizes,
    pub(super) ping_retry_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct SessionParameters {
    pub(super) target: session::Target,
    pub(super) capabilities: Vec<session::Capability>,
}

#[derive(Clone, Debug)]
pub struct BufferSizes {
    pub(super) bridge: String,
    pub(super) ping: String,
    pub(super) main: String,
}
