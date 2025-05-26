use reqwest::header::{self, HeaderMap, HeaderValue};
use std::fmt::Display;
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

impl Display for CustomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Remote Data Error: [")?;
        let mut errors = Vec::new();
        if let Some(reqw_err) = &self.reqw_err {
            errors.push(format!("Request Error: {:?}", reqw_err));
        }
        if let Some(status) = self.status {
            errors.push(format!("Status: {}", status));
        }
        if let Some(value) = &self.value {
            errors.push(format!("Serde Error: {:?}", value));
        }
        write!(f, "{}]", errors.join(", "))
    }
}
