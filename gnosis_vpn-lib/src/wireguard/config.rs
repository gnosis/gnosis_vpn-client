#[derive(Clone, Debug)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub manual_mode: Option<ManualMode>,
    pub allowed_ips: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ManualMode {
    pub public_key: String,
}

impl Config {
    pub(crate) fn new<L, M, S>(listen_port: Option<L>, allowed_ips: Option<M>, manual_mode: Option<S>) -> Self
    where
        L: Into<u16>,
        M: Into<String>,
        S: Into<ManualMode>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
            allowed_ips: allowed_ips.map(Into::into),
            manual_mode: manual_mode.map(Into::into),
        }
    }
}

impl ManualMode {
    pub(crate) fn new<K: Into<String>>(public_key: K) -> Self {
        ManualMode {
            public_key: public_key.into(),
        }
    }
}

impl From<&str> for ManualMode {
    fn from(s: &str) -> Self {
        ManualMode::new(s)
    }
}

impl From<String> for ManualMode {
    fn from(s: String) -> Self {
        ManualMode::new(s)
    }
}
