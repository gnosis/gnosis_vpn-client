use url::Url;

#[derive(Clone, Debug)]
pub struct EntryNode {
    pub endpoint: Url,
    pub api_token: String,
    listen_host: Option<String>,
}

impl EntryNode {
    pub fn new(endpoint: Url, api_token: String, listen_host: Option<String>) -> Self {
        Self {
            endpoint,
            api_token,
            listen_host,
        }
    }
}
