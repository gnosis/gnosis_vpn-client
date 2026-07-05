use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};

use std::fmt::{self, Display};
use std::{io, string};

use crate::dirs;
use crate::shell_command_ext;

pub const WG_INTERFACE: &str = "wg0_gnosisvpn";
pub const WG_MTU: u32 = 1420;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
    #[error("UTF8 conversion error: {0}")]
    FromUtf8Error(#[from] string::FromUtf8Error),
    #[error("TOML serialization error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("error generating wg key")]
    WgGenKey,
    #[error("invalid wireguard key: {0}")]
    InvalidKey(String),
    #[error("Dirs error: {0}")]
    Dirs(#[from] dirs::Error),
    #[error("Shell command error: {0}")]
    ShellCommandExt(#[from] shell_command_ext::Error),
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyPair {
    pub priv_key: String,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub force_private_key: Option<String>,
    /// Source-address filter applied to packets decrypted from the VPN exit
    /// (ingress only). Egress is unconditionally full-tunnel via the OS split
    /// routes and does not consult this value, so a range narrower than the
    /// default `0.0.0.0/0` will drop the exit's NATed return traffic. Defaults to
    /// `0.0.0.0/0` (accept all) when unset.
    pub allowed_ips: Option<String>,
    pub dns: Option<String>,
}

impl Config {
    pub(crate) fn new(
        listen_port: Option<u16>,
        allowed_ips: Option<String>,
        force_private_key: Option<String>,
        dns: Option<String>,
    ) -> Self {
        Config {
            listen_port,
            allowed_ips,
            force_private_key,
            dns,
        }
    }
}

/// Decode a base64-encoded 32-byte WireGuard key (private, public, or preshared)
/// into raw bytes.
///
/// Accepts the exact wire format `wg genkey`/`wg pubkey` emit (standard base64 of
/// 32 raw bytes). Surrounding whitespace/newlines are tolerated, matching the
/// previous behavior where keys were piped through the `wg` binary.
pub(crate) fn decode_key32(key: &str) -> Result<[u8; 32], Error> {
    let bytes = BASE64_STANDARD
        .decode(key.trim())
        .map_err(|e| Error::InvalidKey(format!("base64 decode failed: {e}")))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| Error::InvalidKey(format!("expected 32 key bytes, got {}", v.len())))
}

/// Decode a base64-encoded WireGuard private key into an X25519 secret.
fn decode_secret(priv_key: &str) -> Result<StaticSecret, Error> {
    Ok(StaticSecret::from(decode_key32(priv_key)?))
}

/// Generate a fresh WireGuard private key, base64-encoded exactly as `wg genkey`
/// would emit it. The secret is drawn from OS entropy and never leaves memory.
fn generate_key() -> String {
    let secret = StaticSecret::random();
    BASE64_STANDARD.encode(secret.to_bytes())
}

/// Derive the base64 WireGuard public key for a base64 private key, replacing
/// the former `wg pubkey` shell-out with an in-process Curve25519 basepoint
/// multiplication.
fn public_key(priv_key: &str) -> Result<String, Error> {
    let secret = decode_secret(priv_key)?;
    let public = PublicKey::from(&secret);
    Ok(BASE64_STANDARD.encode(public.as_bytes()))
}

impl WireGuard {
    pub fn new(config: Config, key_pair: KeyPair) -> Self {
        WireGuard { config, key_pair }
    }

    pub async fn from_config(config: Config) -> Result<Self, Error> {
        let priv_key = match config.force_private_key.clone() {
            Some(key) => key,
            None => generate_key(),
        };
        let public_key = public_key(&priv_key)?;
        let key_pair = KeyPair { priv_key, public_key };
        Ok(WireGuard { config, key_pair })
    }
}

impl Display for WireGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WireGuard {{ public_key: {} }}", self.key_pair.public_key)
    }
}

impl fmt::Debug for WireGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WireGuard {{ public_key: {} }}", self.key_pair.public_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a 64-char hex string into 32 bytes (dependency-free test helper).
    fn hex32(s: &str) -> [u8; 32] {
        assert_eq!(s.len(), 64, "expected 64 hex chars");
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("valid hex");
        }
        out
    }

    // RFC 7748 section 6.1 Diffie-Hellman example. WireGuard public-key
    // derivation is exactly X25519(clamp(priv), basepoint), so Alice's published
    // keypair is an independent known-answer test for `public_key`.
    const RFC7748_ALICE_PRIV: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
    const RFC7748_ALICE_PUB: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";

    #[test]
    fn public_key_matches_rfc7748_known_answer() {
        let priv_b64 = BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PRIV));
        let derived = public_key(&priv_b64).expect("valid key");
        assert_eq!(derived, BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PUB)));
    }

    #[test]
    fn public_key_tolerates_trailing_newline() {
        // `wg genkey` output historically carried a trailing newline; the decode
        // path must stay lenient now that we own the parsing.
        let priv_b64 = format!("{}\n", BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PRIV)));
        let derived = public_key(&priv_b64).expect("valid key despite newline");
        assert_eq!(derived, BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PUB)));
    }

    #[test]
    fn generate_key_produces_valid_32_byte_key() {
        let key = generate_key();
        let raw = BASE64_STANDARD.decode(&key).expect("base64");
        assert_eq!(raw.len(), 32);
        // The derived public key must be computable and deterministic.
        assert_eq!(public_key(&key).unwrap(), public_key(&key).unwrap());
    }

    #[test]
    fn generate_key_is_not_constant() {
        assert_ne!(generate_key(), generate_key());
    }

    #[test]
    fn public_key_rejects_non_base64() {
        let err = public_key("this is !!! not base64").unwrap_err();
        assert!(matches!(err, Error::InvalidKey(_)));
    }

    #[test]
    fn public_key_rejects_wrong_length() {
        let short = BASE64_STANDARD.encode([0u8; 16]);
        let err = public_key(&short).unwrap_err();
        assert!(matches!(err, Error::InvalidKey(_)));
    }

    #[tokio::test]
    async fn from_config_derives_public_key_for_forced_private_key() {
        let priv_b64 = BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PRIV));
        let config = Config::new(None, None, Some(priv_b64.clone()), None);
        let wg = WireGuard::from_config(config).await.expect("config accepted");
        assert_eq!(wg.key_pair.priv_key, priv_b64);
        assert_eq!(wg.key_pair.public_key, BASE64_STANDARD.encode(hex32(RFC7748_ALICE_PUB)));
    }
}
