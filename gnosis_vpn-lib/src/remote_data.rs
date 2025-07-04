use reqwest::header::{self, HeaderMap, HeaderValue};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HeaderError {
    #[error("Value cannot be used in http headers")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),
}

pub fn authentication_headers(api_token: &str) -> Result<HeaderMap, HeaderError> {
    let mut headers = json_headers();
    let mut hv_token = HeaderValue::from_str(api_token)?;
    hv_token.set_sensitive(true);
    headers.insert("x-auth-token", hv_token);
    Ok(headers)
}

pub fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers
}
