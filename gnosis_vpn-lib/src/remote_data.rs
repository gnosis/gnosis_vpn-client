use reqwest::header::{self, HeaderMap, HeaderValue};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Value cannot be used in http headers")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Timeout: {0:?}")]
    Timeout(reqwest::Error),
    #[error("Error connecting on specified port: {0:?}")]
    SocketConnect(reqwest::Error),
    #[error("Unauthorized")]
    Unauthorized,
}

pub fn authentication_headers(api_token: &str) -> Result<HeaderMap, Error> {
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

pub fn connect_errors(err: reqwest::Error) -> Error {
    if err.is_connect() {
        Error::SocketConnect(err)
    } else if err.is_timeout() {
        Error::Timeout(err)
    } else {
        err.into()
    }
}

pub fn response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
        Error::Unauthorized
    } else {
        err.into()
    }
}
