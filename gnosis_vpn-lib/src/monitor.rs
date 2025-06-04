use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct PingOptions {
    pub timeout: Duration,
    pub ttl: u32,
    pub seq_count: u16,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ping failed")]
    PingFailed(#[from] ping::Error),
}

impl PingOptions {
    pub fn new(timeout: Duration, ttl: u32, seq_count: u16) -> Self {
        PingOptions {
            timeout,
            ttl,
            seq_count,
        }
    }
}

impl Default for PingOptions {
    fn default() -> Self {
        PingOptions {
            timeout: Duration::from_secs(3),
            ttl: 5,
            seq_count: 1,
        }
    }
}

pub fn ping(opts: &PingOptions) -> Result<(), Error> {
    ping::ping(
        IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1)), // address
        Some(opts.timeout),                       // default timeout 4 sec
        Some(opts.ttl),                           // ttl - number of jumps
        None,                                     // ident - Identifier
        Some(opts.seq_count),                     // Seq Count
        None,                                     // Custom Payload
    )
    .map_err(Error::PingFailed)
}
