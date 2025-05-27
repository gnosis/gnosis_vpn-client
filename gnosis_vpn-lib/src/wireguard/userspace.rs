use crate::wireguard::{ConnectSession, Error, WireGuard};
use boringtun::x25519::{PublicKey, StaticSecret};

use base64::{engine::general_purpose::STANDARD as base64, Engine as _};

#[derive(Debug)]
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
        let stat_sec = StaticSecret::random_from_rng(&mut rand::thread_rng());
        let s = base64.encode(stat_sec.to_bytes());
        Ok(s)
    }

    fn connect_session(&self, _session: &ConnectSession) -> Result<(), Error> {
        Err(Error::NotYetImplemented("connect_session".to_string()))
    }

    fn public_key(&self, priv_key: &str) -> Result<String, Error> {
        let bytes = base64.decode(priv_key)?;
        let arr: [u8; 32] = bytes.as_slice().try_into()?;
        let stat_sec = StaticSecret::from(arr);
        let pubkey = PublicKey::from(&stat_sec);
        let s = base64.encode(pubkey.to_bytes());
        Ok(s)
    }

    fn close_session(&self) -> Result<(), Error> {
        Err(Error::NotYetImplemented("close_session".to_string()))
    }
}
