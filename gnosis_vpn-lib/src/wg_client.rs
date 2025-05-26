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
    #[error("Error converting json to struct: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("Error making http request: {0}")]
    RemoteData(remote_data::CustomError),
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
    let fetch_res = client
        .post(url)
        .json(&json)
        .timeout(std::time::Duration::from_secs(30))
        .headers(headers)
        .send()
        .map(|res| (res.status(), res.json::<serde_json::Value>()));

    match fetch_res {
        Ok((status, Ok(json))) if status.is_success() => {
            let session = serde_json::from_value::<Registration>(json)?;
            Ok(session)
        }
        Ok((status, Ok(json))) => {
            let e = remote_data::CustomError {
                reqw_err: None,
                status: Some(status),
                value: Some(json),
            };
            Err(Error::RemoteData(e))
        }
        Ok((status, Err(e))) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: Some(status),
                value: None,
            };
            Err(Error::RemoteData(e))
        }
        // Err(RemoteData(CustomError { reqw_err: Some(reqwest::Error { kind: Request, url: "http://178.254.33.145:1422/api/v1/clients/register", source: hyper_util::client::legacy::Error(Connect, ConnectError("tcp connect error",
        // Os { code: 111, kind: ConnectionRefused, message: "Connection refused" })) }), status: None, value: None })))
        Err(e) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: None,
                value: None,
            };
            Err(Error::RemoteData(e))
        }
    }
}

pub fn unregister(client: &blocking::Client, input: &Input) -> Result<(), Error> {
    let headers = remote_data::json_headers();
    let mut url = input.endpoint.join("api/v1/clients/unregister")?;
    url.set_port(Some(input.session.port)).map_err(|_| Error::InvalidPort)?;
    let mut json = serde_json::Map::new();
    json.insert("public_key".to_string(), json!(input.public_key));
    tracing::debug!(?headers, body = ?json, ?url, "post unregister client");
    let fetch_res = client
        .post(url)
        .json(&json)
        .timeout(std::time::Duration::from_secs(10))
        .headers(headers)
        .send()
        .map(|res| (res.status(), res.json::<serde_json::Value>()));

    match fetch_res {
        Ok((status, _json)) if status.is_success() => Ok(()),
        Ok((status, Ok(json))) => {
            let e = remote_data::CustomError {
                reqw_err: None,
                status: Some(status),
                value: Some(json),
            };
            Err(Error::RemoteData(e))
        }
        Ok((status, Err(e))) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: Some(status),
                value: None,
            };
            Err(Error::RemoteData(e))
        }
        Err(e) => {
            let e = remote_data::CustomError {
                reqw_err: Some(e),
                status: None,
                value: None,
            };
            Err(Error::RemoteData(e))
        }
    }
}

impl Display for Registration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WgRegistration[{}]", self.ip)
    }
}
