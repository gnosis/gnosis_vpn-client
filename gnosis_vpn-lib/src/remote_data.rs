use backon::ExponentialBuilder;
use reqwest::header::{self, HeaderMap, HeaderValue};

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
