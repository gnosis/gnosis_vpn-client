use thiserror::Error;
use url::Url;

use std::time::Duration;

use crate::session::Session;

#[derive(Clone, Debug)]
pub struct EntryNode {
    pub endpoint: Url,
    pub api_token: String,
    pub listen_host: String,
    pub session_timeout: Duration,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Endpoint URL does not contain a valid host")]
    NoHost,
}

impl EntryNode {
    pub fn new(endpoint: &Url, api_token: &str, listen_host: &str, session_timeout: &Duration) -> Self {
        Self {
            endpoint: endpoint.clone(),
            api_token: api_token.to_string(),
            listen_host: listen_host.to_string(),
            session_timeout: *session_timeout,
        }
    }

    pub fn conflicts_listen_host(&self, session: &Session) -> bool {
        self.listen_host.ends_with(&session.port.to_string())
    }

    pub fn endpoint_with_port(&self, port: u16) -> Result<String, Error> {
        let host = self.endpoint.host_str().ok_or(Error::NoHost)?;
        Ok(format!("{host}:{port}"))
    }
}
