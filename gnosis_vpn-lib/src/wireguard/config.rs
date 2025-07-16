#[derive(Clone, Debug)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub force_private_key: Option<String>,
    pub allowed_ips: Option<String>,
}

impl Config {
    pub(crate) fn new<L, M, S>(listen_port: Option<L>, allowed_ips: Option<M>, force_private_key: Option<S>) -> Self
    where
        L: Into<u16>,
        M: Into<String>,
        S: Into<String>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
            allowed_ips: allowed_ips.map(Into::into),
            force_private_key: force_private_key.map(Into::into),
        }
    }
}
