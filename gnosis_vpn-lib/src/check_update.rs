use pgp::{Deserializable, SignedPublicKey, StandaloneSignature};
use reqwest::Client;
use thiserror::Error;

use std::io::Cursor;

use crate::command::Manifest;

const PUBLIC_KEY: &str = include_str!("../../gnosisvpn-public-key.asc");

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MANIFEST_FILENAME: &str = "linux-amd64.json";

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MANIFEST_FILENAME: &str = "linux-arm64.json";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MANIFEST_FILENAME: &str = "macos-arm64.json";

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error making http request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("PGP error: {0}")]
    Pgp(#[from] pgp::errors::Error),
    #[error("Error parsing manifest: {0}")]
    Json(#[from] serde_json::Error),
}

fn verify_and_parse(manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<Manifest, Error> {
    let (public_key, _) = SignedPublicKey::from_armor_single(Cursor::new(PUBLIC_KEY))?;
    let (sig, _) = StandaloneSignature::from_armor_single(Cursor::new(sig_bytes))?;
    sig.verify(&public_key, manifest_bytes)?;
    let manifest = serde_json::from_slice(manifest_bytes)?;
    Ok(manifest)
}

pub async fn download(client: &Client, manifest_base_url: &str) -> Result<Manifest, Error> {
    let sig_filename = MANIFEST_FILENAME.replace(".json", ".json.asc");
    let manifest_url = url::Url::parse(&format!("{}{}", manifest_base_url, MANIFEST_FILENAME))?;
    let sig_url = url::Url::parse(&format!("{}{}", manifest_base_url, sig_filename))?;

    tracing::debug!(?manifest_url, ?sig_url, "downloading update manifest and signature");

    let manifest_bytes = client
        .get(manifest_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let sig_bytes = client.get(sig_url).send().await?.error_for_status()?.bytes().await?;

    verify_and_parse(&manifest_bytes, &sig_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES_DIR}/{name}")).expect("fixture file not found")
    }

    fn verify_fixture(manifest_file: &str) {
        let sig_file = manifest_file.replace(".json", ".json.asc");
        let manifest_bytes = fixture(manifest_file);
        let sig_bytes = fixture(&sig_file);
        let result = verify_and_parse(&manifest_bytes, &sig_bytes);
        assert!(
            result.is_ok(),
            "verification failed for {manifest_file}: {:?}",
            result.err()
        );
        let manifest = result.unwrap();
        assert_eq!(manifest.schema_version, 1, "schema_version should be 1");
        let stable = manifest.channels.stable.expect("stable channel should exist");
        assert!(!stable.version.is_empty(), "stable version should not be empty");
    }

    #[test]
    fn verify_linux_amd64() {
        verify_fixture("linux-amd64.json");
    }

    #[test]
    fn verify_linux_arm64() {
        verify_fixture("linux-arm64.json");
    }

    #[test]
    fn verify_macos_arm64() {
        verify_fixture("macos-arm64.json");
    }

    #[test]
    fn rejects_tampered_manifest() {
        let mut manifest_bytes = fixture("linux-amd64.json");
        let sig_bytes = fixture("linux-amd64.json.asc");
        // flip a byte in the middle to simulate tampering
        let mid = manifest_bytes.len() / 2;
        manifest_bytes[mid] ^= 0xff;
        let result = verify_and_parse(&manifest_bytes, &sig_bytes);
        assert!(result.is_err(), "tampered manifest should fail verification");
    }

    #[test]
    fn rejects_mismatched_signature() {
        let manifest_bytes = fixture("linux-amd64.json");
        let wrong_sig = fixture("linux-arm64.json.asc");
        let result = verify_and_parse(&manifest_bytes, &wrong_sig);
        assert!(result.is_err(), "wrong signature should fail verification");
    }
}
