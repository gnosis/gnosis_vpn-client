#[derive(Clone, Debug)]
pub struct Config {
    listen_port: Option<u16>,
    manual_mode: Option<ManualMode>,
}

#[derive(Clone, Debug)]
pub struct ManualMode {
    public_key: String,
}

impl Config {
    pub fn new(listen_port: &Option<u16>, manual_mode: &Option<ManualMode>) -> Self {
        Config {
            listen_port: listen_port.clone(),
            manual_mode: manual_mode.clone(),
        }
    }
}

impl ManualMode {
    pub fn new(public_key: &str) -> Self {
        ManualMode {
            public_key: public_key.to_string(),
        }
    }
}
