use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use url::Url;

use std::fmt::{self, Display};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use gnosis_vpn_ipc::{ApiVersion, ExitVersions, Health};

use crate::remote_data;

#[derive(Clone, Serialize, Deserialize)]
pub struct Registration {
    public_key: String,
    ip: Ipv4Addr,
    newly_registered: bool,
    server_public_key: String,
    preshared_key: String,
}

/// The preshared key is a secret and `Up` is debug-logged with its registration.
impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("public_key", &self.public_key)
            .field("ip", &self.ip)
            .field("newly_registered", &self.newly_registered)
            .field("server_public_key", &self.server_public_key)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Input {
    public_key: String,
    socket_addr: SocketAddr,
    timeout: Duration,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error connecting on specified port: {0:?}")]
    SocketConnect(reqwest::Error),
    #[error("Connection reset by peer: {0:?}")]
    ConnectionReset(reqwest::Error),
    #[error("Request timed out: {0:?}")]
    Timeout(reqwest::Error),
    #[error("Registration not found")]
    RegistrationNotFound,
}

impl Input {
    pub fn new(public_key: String, socket_addr: SocketAddr, timeout: Duration) -> Self {
        Input {
            public_key,
            socket_addr,
            timeout,
        }
    }
}

impl Registration {
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    pub fn address(&self) -> String {
        format!("{}/32", self.ip)
    }

    pub fn server_public_key(&self) -> String {
        self.server_public_key.clone()
    }

    pub fn preshared_key(&self) -> String {
        self.preshared_key.clone()
    }
}

/// The only exit API this client speaks; every route below lives under it.
pub const API: ApiVersion = ApiVersion::new(1);

/// What `/versions` publishes; `latest` is ignored, as no decision depends on it.
#[derive(Deserialize)]
struct PublishedVersions {
    versions: Vec<ApiVersion>,
    server: Option<semver::Version>,
}

pub async fn versions(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<ExitVersions, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}/versions",
            host = socket_addr.ip(),
            port = socket_addr.port()
        )
        .as_str(),
    )?;
    tracing::debug!(?headers, ?url, "get server versions");
    let resp = client
        .get(url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<PublishedVersions>()
        .await?;

    Ok(ExitVersions {
        api: resp.versions,
        server: resp.server,
    })
}

pub async fn ping(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<(), Error> {
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/ping",
            host = socket_addr.ip(),
            port = socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    tracing::debug!(?url, "ping exit server");
    client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(connect_errors)?
        .error_for_status()?;

    Ok(())
}

pub async fn health(client: &Client, socket_addr: SocketAddr, timeout: Duration) -> Result<Health, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/status",
            host = socket_addr.ip(),
            port = socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    tracing::debug!(?headers, ?url, "get server health");
    let resp = client
        .get(url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<Health>()
        .await?;

    Ok(resp)
}

pub async fn register(client: &Client, input: &Input) -> Result<Registration, Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/clients/register",
            host = input.socket_addr.ip(),
            port = input.socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    let json = json!({
        "public_key": input.public_key,
    });
    tracing::debug!(?headers, body = ?json, ?url, "post register client");
    let resp = client
        .post(url)
        .json(&json)
        .timeout(input.timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()?
        .json::<Registration>()
        .await?;

    Ok(resp)
}

pub async fn unregister(client: &Client, input: &Input) -> Result<(), Error> {
    let headers = remote_data::json_headers();
    let url = Url::parse(
        format!(
            "http://{host}:{port}{api}/clients/unregister",
            host = input.socket_addr.ip(),
            port = input.socket_addr.port(),
            api = API.path()
        )
        .as_str(),
    )?;
    let mut json = serde_json::Map::new();
    json.insert("public_key".to_string(), json!(input.public_key));
    tracing::debug!(?headers, body = ?json, ?url, "post unregister client");
    client
        .post(url)
        .json(&json)
        .timeout(input.timeout)
        .headers(headers)
        .send()
        .await
        // connection error checks happen before response
        .map_err(connect_errors)?
        .error_for_status()
        // response error checks happen after response
        .map_err(response_errors)?;

    Ok(())
}

fn connect_errors(err: reqwest::Error) -> Error {
    if err.is_connect() {
        Error::SocketConnect(err)
    } else if err.is_timeout() {
        // Before `is_request`, which also covers timeouts and would call them resets.
        Error::Timeout(err)
    } else if err.is_request() {
        Error::ConnectionReset(err)
    } else {
        err.into()
    }
}

fn response_errors(err: reqwest::Error) -> Error {
    if err.status() == Some(reqwest::StatusCode::NOT_FOUND) {
        Error::RegistrationNotFound
    } else {
        err.into()
    }
}

impl Display for Registration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WgRegistration {{ ip: {} }}", self.ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_debug_hides_the_preshared_key() {
        let registration: Registration = serde_json::from_value(json!({
            "public_key": "pub",
            "ip": "10.128.0.5",
            "newly_registered": false,
            "server_public_key": "srv",
            "preshared_key": "secret-psk",
        }))
        .expect("valid registration");
        let printed = format!("{registration:?}");
        assert!(printed.contains("pub"));
        assert!(!printed.contains("secret-psk"), "{printed}");
    }
}
