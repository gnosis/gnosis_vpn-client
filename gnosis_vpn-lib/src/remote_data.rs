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

/// Resolves the IPv4 addresses for the host and port specified in the provided URL.
pub async fn resolve_ips(url: &url::Url) -> Result<Vec<Ipv4Addr>, Error> {
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
