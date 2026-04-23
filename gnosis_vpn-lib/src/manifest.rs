use pgp::{Deserializable, SignedPublicKey, StandaloneSignature};
use reqwest::Client;
use thiserror::Error;

use std::io::Cursor;

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
compile_error!("unsupported platform: no update manifest available for this target");

const MANIFEST_BASE_URL: &str = "https://download.gnosisvpn.io/manifest/";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error making http request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Error parsing url: {0}")]
    Url(#[from] url::ParseError),
    #[error("Manifest signature invalid: {0}")]
    SignatureInvalid(#[from] pgp::errors::Error),
    #[error("Error parsing manifest: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn download(client: &Client) -> Result<serde_json::Value, Error> {
    let sig_filename = MANIFEST_FILENAME.replace(".json", ".asc");
    let manifest_url = url::Url::parse(&format!("{}{}", MANIFEST_BASE_URL, MANIFEST_FILENAME))?;
    let sig_url = url::Url::parse(&format!("{}{}", MANIFEST_BASE_URL, sig_filename))?;

    tracing::debug!(?manifest_url, ?sig_url, "downloading update manifest and signature");

    let manifest_bytes = client
        .get(manifest_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let sig_bytes = client
        .get(sig_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let (public_key, _) = SignedPublicKey::from_armor_single(Cursor::new(PUBLIC_KEY))?;
    let (sig, _) = StandaloneSignature::from_armor_single(Cursor::new(sig_bytes.as_ref()))?;
    sig.verify(&public_key, manifest_bytes.as_ref())?;

    let manifest = serde_json::from_slice(&manifest_bytes)?;
    Ok(manifest)
}
