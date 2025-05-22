use std::time::Duration;
use url::Url;

#[derive(Clone, Debug)]
pub struct EntryNode {
    pub endpoint: Url,
    pub api_token: String,
    pub listen_host: String,
    pub session_timeout: Duration,
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
}
