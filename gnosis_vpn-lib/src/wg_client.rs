use reqwest::blocking;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::Ipv4Addr;
use thiserror::Error;

use crate::entry_node::EntryNode;
use crate::remote_data;
use crate::session::Session;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Register {
    public_key: String,
    ip: Ipv4Addr,
    newly_registered: bool,
    server_public_key: String,
}

pub struct RegisterInput {
    public_key: String,
    entry_node: EntryNode,
    session: Session,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Error parsing url")]
    Url(#[from] url::ParseError),
    #[error("Error converting json to struct")]
    Deserialize(#[from] serde_json::Error),
    #[error("Error making http request")]
    RemoteData(remote_data::CustomError),
    #[error("Invalid port")]
    InvalidPort,
}

pub fn register(client: &blocking::Client, input: &RegisterInput) -> Result<Register, Error> {
    let headers = remote_data::json_headers();
    let mut url = input.entry_node.endpoint.join("api/v1/clients/register")?;
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
            let session = serde_json::from_value::<Register>(json)?;
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
