use url::Url;

#[derive(Clone, Debug)]
pub struct EntryNode {
    pub endpoint: Url,
    pub api_token: String,
    pub listen_host: String,
}

impl EntryNode {
    pub fn new(endpoint: &Url, api_token: &str, listen_host: &str) -> Self {
        Self {
            endpoint: endpoint.clone(),
            api_token: api_token.to_string(),
            listen_host: listen_host.to_string(),
        }
    }
}
