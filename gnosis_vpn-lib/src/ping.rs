use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PingOptions {
    pub address: IpAddr,
    pub timeout: Duration,
    pub ttl: u32,
    pub seq_count: u16,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ping failed")]
    PingFailed(#[from] ping::Error),
}

impl Default for PingOptions {
    fn default() -> Self {
        PingOptions {
            address: IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1)),
            timeout: Duration::from_secs(15),
            ttl: 6,
            seq_count: 1,
        }
    }
}

#[tracing::instrument(name = "ping", ret)]
pub fn ping(opts: &PingOptions) -> Result<Duration, Error> {
    let mut builder = ping::new(opts.address);
    let mut ping = builder.timeout(opts.timeout).ttl(opts.ttl).seq_cnt(opts.seq_count);
    #[cfg(target_os = "linux")]
    {
        ping = ping.socket_type(ping::RAW);
    }
    #[cfg(target_os = "macos")]
    {
        ping = ping.socket_type(ping::DGRAM);
    }
    ping.send().map(|p| p.rtt).map_err(Error::from)
}
