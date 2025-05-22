use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ping failed")]
    PingFailed(#[from] ping::Error),
}

pub fn ping() -> Result<(), Error> {
    ping::ping(
        IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1)), // address
        Some(Duration::from_secs(3)),             // default timeout 4 sec
        Some(5),                                  // ttl - number of jumps
        None,                                     // ident - Identifier
        Some(1),                                  // Seq Count
        None,                                     // Custom Payload
    )
    .map_err(Error::PingFailed)
}
