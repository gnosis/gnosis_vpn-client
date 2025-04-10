use thiserror::Error;
use reqwest::header::{self, HeaderMap, HeaderValue};

#[derive(Error, Debug)]
pub enum HeaderError {
    #[error("Value cannot be used in http headers")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderName),
}

pub fn authentication_headers(api_token: &str) -> Result<HeaderMap, HeaderError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let mut hv_token = HeaderValue::from_str(api_token)?;
    hv_token.set_sensitive(true);
    headers.insert("x-auth-token", hv_token);
    Ok(headers)
}
