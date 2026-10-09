use backon::ExponentialBuilder;
use reqwest::header::{self, HeaderMap, HeaderValue};
use thiserror::Error;
use tokio::net;

use std::io;
use std::net::Ipv4Addr;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Host not found in the provided URL")]
    NoHost,
    #[error("Port not found or unknown in the provided URL")]
    UnknownPort,
    #[error("Host has no IPv4 address")]
    NoIpv4,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

pub fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers
}

/// Creates a backoff strategy with exponential backoff and jitter, suitable for retrying remote
/// data fetches with long delays.
pub fn backoff_expo_long_delay() -> ExponentialBuilder {
    ExponentialBuilder::new()
        .with_min_delay(std::time::Duration::from_secs(10))
        .with_max_delay(std::time::Duration::from_secs(60))
        .with_factor(2.0)
        .with_jitter()
}

/// Creates a backoff strategy with exponential backoff and jitter, suitable for retrying remote
/// data fetches with short delays.
pub fn backoff_expo_short_delay() -> ExponentialBuilder {
    ExponentialBuilder::new()
        .with_min_delay(std::time::Duration::from_secs(1))
        .with_max_delay(std::time::Duration::from_secs(10))
        .with_factor(2.0)
        .with_jitter()
}

/// The one IPv4 address the whole session pins Blokli to (client, killswitch exemption, WAN probe).
pub async fn resolve_blokli_ip(url: &url::Url) -> Result<Ipv4Addr, Error> {
    resolve_ips(url).await?.first().copied().ok_or(Error::NoIpv4)
}

/// Resolves the IPv4 addresses for the host and port specified in the provided URL.
async fn resolve_ips(url: &url::Url) -> Result<Vec<Ipv4Addr>, Error> {
    let host = url.host_str().ok_or(Error::NoHost)?;
    let port = url.port_or_known_default().ok_or(Error::UnknownPort)?;
    let addr_str = format!("{}:{}", host, port);
    let mut ips = Vec::new();
    for addr in net::lookup_host(addr_str).await? {
        match addr.ip() {
            std::net::IpAddr::V4(ipv4) => ips.push(ipv4),
            std::net::IpAddr::V6(_) => continue,
        }
    }
    Ok(ips)
}
