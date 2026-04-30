use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use pgp::{Deserializable, SignedPublicKey, StandaloneSignature};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_with::{hex::Hex, serde_as};
use std::fmt;
use std::io::Cursor;
use url::Url;

pub type Timestamp = DateTime<Utc>;

#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hash(#[serde_as(as = "Hex")] pub [u8; 32]);

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

const PUBLIC_KEY: &str = include_str!("../../gnosisvpn-public-key.asc");
const MANIFEST_BASE_URL: &str = "https://download.gnosisvpn.io/manifests/";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MANIFEST_FILENAME: &str = "linux-amd64.json";

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MANIFEST_FILENAME: &str = "linux-arm64.json";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MANIFEST_FILENAME: &str = "macos-arm64.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub generated_at: String,
    pub channels: ManifestChannels,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestChannels {
    pub stable: Option<ChannelRelease>,
    pub snapshot: Option<ChannelRelease>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelRelease {
    pub version: String,
    pub published_at: Timestamp,
    pub download_url: Url,
    pub size_bytes: ByteSize,
    pub sha256: Hash,
    pub artifact_signature: String,
    pub release_notes: String,
    pub min_os_version: String,
    pub min_app_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Manifest integrity error: {0}")]
    Integrity(String),
    #[error("Update check error: {0}")]
    Other(String),
}

fn verify_and_parse(manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<Manifest, Error> {
    let (public_key, _) =
        SignedPublicKey::from_armor_single(Cursor::new(PUBLIC_KEY)).map_err(|e| Error::Integrity(e.to_string()))?;
    let (sig, _) =
        StandaloneSignature::from_armor_single(Cursor::new(sig_bytes)).map_err(|e| Error::Integrity(e.to_string()))?;
    sig.verify(&public_key, manifest_bytes).map_err(|e| Error::Integrity(e.to_string()))?;
    serde_json::from_slice(manifest_bytes).map_err(|e| Error::Integrity(e.to_string()))
}

pub async fn download(client: &Client) -> Result<Manifest, Error> {
    let sig_filename = MANIFEST_FILENAME.replace(".json", ".json.asc");
    let base = url::Url::parse(MANIFEST_BASE_URL).map_err(|e| Error::Other(e.to_string()))?;
    let manifest_url = base.join(MANIFEST_FILENAME).map_err(|e| Error::Other(e.to_string()))?;
    let sig_url = base.join(&sig_filename).map_err(|e| Error::Other(e.to_string()))?;

    tracing::debug!(?manifest_url, ?sig_url, "downloading update manifest and signature");

    let manifest_bytes = client
        .get(manifest_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| Error::Other(e.to_string()))?;

    let sig_bytes = client
        .get(sig_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| Error::Other(e.to_string()))?;

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
    fn deserializes_all_fixtures() {
        for name in ["linux-amd64.json", "linux-arm64.json", "macos-arm64.json"] {
            let bytes = fixture(name);
            let manifest: Manifest =
                serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
            let stable = manifest.channels.stable.expect("stable channel");
            assert_eq!(stable.sha256.0.len(), 32);
            assert!(stable.size_bytes.as_u64() > 0);
            assert!(stable.published_at.timestamp() > 0);
            assert!(!stable.min_os_version.is_empty());
        }
    }

    #[test]
    fn rejects_mismatched_signature() {
        let manifest_bytes = fixture("linux-amd64.json");
        let wrong_sig = fixture("linux-arm64.json.asc");
        let result = verify_and_parse(&manifest_bytes, &wrong_sig);
        assert!(result.is_err(), "wrong signature should fail verification");
    }
}
