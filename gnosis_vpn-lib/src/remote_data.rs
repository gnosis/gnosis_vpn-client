use reqwest::header::{self, HeaderMap, HeaderValue};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HeaderError {
    #[error("Value cannot be used in http headers")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),
}

#[derive(Debug)]
pub struct CustomError {
    pub reqw_err: Option<reqwest::Error>,
    pub status: Option<reqwest::StatusCode>,
    pub value: Option<serde_json::Value>,
}

pub fn authentication_headers(api_token: &str) -> Result<HeaderMap, HeaderError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let mut hv_token = HeaderValue::from_str(api_token)?;
    hv_token.set_sensitive(true);
    headers.insert("x-auth-token", hv_token);
    Ok(headers)
}
