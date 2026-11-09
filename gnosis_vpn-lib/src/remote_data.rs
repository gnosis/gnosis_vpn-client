use backon::ExponentialBuilder;
use edgli::{BlokliDnsOverride, BlokliEndpoint};
use reqwest::header::{self, HeaderMap, HeaderValue};
use thiserror::Error;
use tokio::net;

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

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
    #[error("Blokli query failed: {0}")]
    Blokli(String),
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

/// Every distinct IPv4 address the host in `url` resolves to, in resolver order.
///
/// Hosts behind a load balancer or global accelerator answer with several addresses and the
/// resolver picks one per connection, so the killswitch has to exempt all of them, not just the
/// first one.
pub async fn resolve_ipv4s(url: &url::Url) -> Result<Vec<Ipv4Addr>, Error> {
    let mut ips = resolve_ips(url).await?;
    ips.dedup();
    if ips.is_empty() {
        return Err(Error::NoIpv4);
    }
    Ok(ips)
}

/// Blokli's `chainInfo.network`, through the already pinned address.
pub async fn query_blokli_network(url: &url::Url, ip: Ipv4Addr, request_timeout: Duration) -> Result<String, Error> {
    let endpoint = BlokliEndpoint::new(url.clone())
        .with_request_timeout(request_timeout)
        .with_dns_override(BlokliDnsOverride::new(IpAddr::V4(ip), None));
    edgli::query_network(&endpoint)
        .await
        .map_err(|e| Error::Blokli(format!("{e:#}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An IP literal needs no DNS, which is the one case a resolver test can assert offline.
    #[tokio::test]
    async fn ip_literal_host_resolves_to_itself() {
        let url = url::Url::parse("https://203.0.113.7:8443").expect("valid url");
        assert_eq!(resolve_ipv4s(&url).await.unwrap(), vec![Ipv4Addr::new(203, 0, 113, 7)]);
    }

    #[tokio::test]
    async fn blokli_and_relayer_resolution_agree_for_a_single_address() {
        let url = url::Url::parse("https://203.0.113.7:8443").expect("valid url");
        assert_eq!(
            resolve_blokli_ip(&url).await.unwrap(),
            resolve_ipv4s(&url).await.unwrap()[0]
        );
    }

    #[tokio::test]
    async fn url_without_a_known_port_is_rejected() {
        let url = url::Url::parse("foo://relayer.example/paymaster").expect("valid url");
        assert!(matches!(resolve_ipv4s(&url).await, Err(Error::UnknownPort)));
    }
}
