#[derive(Debug)]
pub struct Config {
    listen_port: Option<u16>,
}

impl Config {
    pub fn new(listen_port: &Option<u16>) -> Self {
        Config {
            listen_port: listen_port.clone(),
        }
    }
}
