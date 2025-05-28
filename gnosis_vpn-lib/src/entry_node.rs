use std::fmt::{self, Display};
use std::time::Duration;
use url::Url;

use crate::session::Session;

#[derive(Clone, Debug)]
pub struct EntryNode {
    pub endpoint: Url,
    pub api_token: String,
    pub listen_host: String,
    pub session_timeout: Duration,
    pub api_version: APIVersion,
}

#[derive(Clone, Debug)]
pub enum APIVersion {
    V3,
    V4,
}

impl EntryNode {
    pub fn new(
        endpoint: Url,
        api_token: String,
        listen_host: String,
        session_timeout: Duration,
        api_version: APIVersion,
    ) -> Self {
        Self {
            endpoint,
            api_token,
            listen_host,
            session_timeout,
            api_version,
        }
    }

    pub fn conflicts_listen_host(&self, session: &Session) -> bool {
        self.listen_host.ends_with(&session.port.to_string())
    }
}

impl Display for APIVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}

impl AsRef<str> for APIVersion {
    fn as_ref(&self) -> &str {
        match self {
            APIVersion::V3 => "v3",
            APIVersion::V4 => "v4",
        }
    }
}
