use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use url::Url;

use std::fmt::{self, Display};
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::remote_data;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    public_key: String,
    ip: Ipv4Addr,
    newly_registered: bool,
    server_public_key: String,
}

#[derive(Clone, Debug)]
pub struct Input {
    public_key: String,
    port: u16,
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
    #[error("Invalid port")]
    InvalidPort,
    #[error("Registration not found")]
    RegistrationNotFound,
}

impl Input {
    pub fn new(public_key: String, port: u16, timeout: Duration) -> Self {
        Input {
            public_key,
            port,
            timeout,
        }
    }
}

impl Registration {
    pub fn address(&self) -> String {
        format!("{}/32", self.ip)
    }

    pub fn server_public_key(&self) -> String {
        self.server_public_key.clone()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Health {
    slots: Slots,
    load_avg: LoadAvg,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Slots {
    available: u32,
    connected: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoadAvg {
    one: f32,
    five: f32,
    fifteen: f32,
    nproc: u16,
}

pub async fn health(client: &Client, input: &Input) -> Result<Health, Error> {
    let headers = remote_data::json_headers();
    let mut url = Url::parse("http://localhost/api/v1/status")?;
    url.set_port(Some(input.port)).map_err(|_| Error::InvalidPort)?;
    tracing::debug!(?headers, body = ?json, ?url, "get server health");
    let resp = client
        .get(url)
        .timeout(input.timeout)
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
    let mut url = Url::parse("http://localhost/api/v1/clients/register")?;
    url.set_port(Some(input.port)).map_err(|_| Error::InvalidPort)?;
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
    let mut url = Url::parse("http://localhost/api/v1/clients/unregister")?;
    url.set_port(Some(input.port)).map_err(|_| Error::InvalidPort)?;
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
