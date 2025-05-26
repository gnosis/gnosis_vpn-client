use reqwest::blocking;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::{self, Display};
use std::net::Ipv4Addr;
use thiserror::Error;
use url::Url;

use crate::remote_data;
use crate::session::Session;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    public_key: String,
    ip: Ipv4Addr,
    newly_registered: bool,
    server_public_key: String,
}

pub struct Input {
    public_key: String,
    endpoint: Url,
    session: Session,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Error making http request: {0:?}")]
    Request(#[from] reqwest::Error),
    #[error("Error connecting on specified port")]
    SocketConnect,
    #[error("Invalid port")]
    InvalidPort,
}

impl Input {
    pub fn new(public_key: &str, endpoint: &Url, session: &Session) -> Self {
        Input {
            public_key: public_key.to_string(),
            endpoint: endpoint.clone(),
            session: session.clone(),
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

pub fn register(client: &blocking::Client, input: &Input) -> Result<Registration, Error> {
    let headers = remote_data::json_headers();
    let mut url = input.endpoint.join("api/v1/clients/register")?;
    url.set_port(Some(input.session.port)).map_err(|_| Error::InvalidPort)?;
    let mut json = serde_json::Map::new();
    json.insert("public_key".to_string(), json!(input.public_key));
    tracing::debug!(?headers, body = ?json, ?url, "post register client");
    let resp = client
        .post(url)
        .json(&json)
        .timeout(std::time::Duration::from_secs(30))
        .headers(headers)
        .send()
        .map_err(|err| {
            if err.is_connect() {
                Error::SocketConnect
            } else {
                err.into()
            }
        })?
        .json::<Registration>()?;

    Ok(resp)
}

pub fn unregister(client: &blocking::Client, input: &Input) -> Result<(), Error> {
    let headers = remote_data::json_headers();
    let mut url = input.endpoint.join("api/v1/clients/unregister")?;
    url.set_port(Some(input.session.port)).map_err(|_| Error::InvalidPort)?;
    let mut json = serde_json::Map::new();
    json.insert("public_key".to_string(), json!(input.public_key));
    tracing::debug!(?headers, body = ?json, ?url, "post unregister client");
    client
        .post(url)
        .json(&json)
        .timeout(std::time::Duration::from_secs(10))
        .headers(headers)
        .send()
        .map_err(|err| {
            if err.is_connect() {
                Error::SocketConnect
            } else {
                err.into()
            }
        })?
        .error_for_status()?;
    Ok(())
}

impl Display for Registration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WgRegistration[{}]", self.ip)
    }
}
