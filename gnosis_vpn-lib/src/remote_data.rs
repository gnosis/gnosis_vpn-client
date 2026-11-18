use backon::ExponentialBuilder;
use edgli::{BlokliDnsOverride, BlokliEndpoint, FailureKind};
use gnosis_vpn_ipc::StartupIssueKind;
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
    #[error("resolving the blokli host timed out")]
    ResolveTimeout,
    #[error("Blokli query failed: {message}")]
    Blokli { kind: FailureKind, message: String },
}

impl Error {
    /// How the app explains this failure while it blocks startup.
    pub fn startup_issue_kind(&self) -> StartupIssueKind {
        match self {
            // Only the system resolver does IO here.
            Error::IO(_) | Error::NoIpv4 | Error::ResolveTimeout => StartupIssueKind::Unreachable,
            Error::Blokli { kind, .. } => crate::core::startup_issue::kind(*kind),
            Error::NoHost | Error::UnknownPort => StartupIssueKind::Other,
        }
    }
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
    resolve_blokli_ips(url).await?.first().copied().ok_or(Error::NoIpv4)
}

/// Every address, so a round-robin answer does not read as Blokli moving.
pub async fn resolve_blokli_ips(url: &url::Url) -> Result<Vec<Ipv4Addr>, Error> {
    resolve_ips(url).await
}

/// Blokli's `chainInfo.network`, through the already pinned address.
pub async fn query_blokli_network(url: &url::Url, ip: Ipv4Addr, request_timeout: Duration) -> Result<String, Error> {
    let endpoint = BlokliEndpoint::new(url.clone())
        .with_request_timeout(request_timeout)
        .with_dns_override(BlokliDnsOverride::new(IpAddr::V4(ip), None));
    edgli::query_network(&endpoint).await.map_err(|e| Error::Blokli {
        kind: e.failure_kind(),
        message: format!("{e:#}"),
    })
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

    #[test]
    fn blokli_failures_keep_their_kind() {
        let error = Error::Blokli {
            kind: FailureKind::ServiceUnavailable,
            message: "503 Service Unavailable".to_string(),
        };

        assert_eq!(error.startup_issue_kind(), StartupIssueKind::ServiceUnavailable);
        assert_eq!(error.to_string(), "Blokli query failed: 503 Service Unavailable");
    }

    #[test]
    fn a_failed_lookup_is_unreachable_and_a_bad_url_is_not() {
        assert_eq!(
            Error::IO(io::Error::other("no such host")).startup_issue_kind(),
            StartupIssueKind::Unreachable
        );
        assert_eq!(Error::NoIpv4.startup_issue_kind(), StartupIssueKind::Unreachable);
        assert_eq!(Error::NoHost.startup_issue_kind(), StartupIssueKind::Other);
    }
}
