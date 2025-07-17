use crate::wireguard::{ConnectSession, Error, WireGuard};

#[derive(Clone, Debug)]
pub struct UserSpace {}

pub fn available() -> Result<bool, Error> {
    Err(Error::NotYetImplemented("userspace".to_string()))
}

impl UserSpace {
    pub fn new() -> Self {
        UserSpace {}
    }
}

impl WireGuard for UserSpace {
    fn generate_key(&self) -> Result<String, Error> {
        Err(Error::NotYetImplemented("userspace".to_string()))
    }

    fn connect_session(&self, _session: &ConnectSession) -> Result<(), Error> {
        Err(Error::NotYetImplemented("connect_session".to_string()))
    }

    fn public_key(&self, _priv_key: &str) -> Result<String, Error> {
        Err(Error::NotYetImplemented("connect_session".to_string()))
    }

    fn close_session(&self) -> Result<(), Error> {
        Err(Error::NotYetImplemented("close_session".to_string()))
    }
}
