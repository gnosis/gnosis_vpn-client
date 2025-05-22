use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ping failed")]
    PingFailed(#[from] ping::Error),
}

pub fn ping() -> Result<(), Error> {
    let res = ping::ping(
        IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1)), // address
        Some(Duration::from_secs(10)),            // timeout
        None,                                     // ttl - number of jumps
        None,                                     // ident - Identifier
        Some(1),                                  // Seq Count
        None,                                     // Custom Payload
    )?;
    tracing::info!(?res, "ping");
    Ok(res)
}
