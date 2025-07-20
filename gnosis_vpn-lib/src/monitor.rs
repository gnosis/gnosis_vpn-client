use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug)]
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
            timeout: Duration::from_secs(4),
            ttl: 5,
            seq_count: 1,
        }
    }
}

impl Error {
    pub fn would_block(&self) -> bool {
        match self {
            Error::PingFailed(ping::Error::IoError { error: err }) => err.kind() == std::io::ErrorKind::WouldBlock,
            _ => false,
        }
    }
}

pub fn ping(opts: &PingOptions) -> Result<(), Error> {
    ping::ping(
        opts.address,
        Some(opts.timeout),   // default timeout 4 sec
        Some(opts.ttl),       // ttl - number of jumps
        None,                 // ident - Identifier
        Some(opts.seq_count), // Seq Count
        None,                 // Custom Payload
    )
    .map_err(Error::PingFailed)
}
