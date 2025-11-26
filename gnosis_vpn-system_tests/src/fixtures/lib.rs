use std::{path::PathBuf, time::Duration};

use tokio::time::Instant;
use tracing::{debug, warn};
use url::Url;

pub fn env_string_with_default(key: &'static str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) => v,
        Err(_) => default.to_string(),
    }
}

pub async fn wait_for_condition<T, F, Fut>(
    label: &str,
    timeout: Duration,
    interval: Duration,
    mut check: F,
) -> anyhow::Result<T>
where
    T: std::fmt::Debug,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<T>>>,
{
    let start = Instant::now();
    loop {
        debug!("checking condition for {label}");
        if let Some(result) = check().await? {
            return Ok(result);
        }

        if start.elapsed() > timeout {
            Err(anyhow::anyhow!("timeout while waiting for {label}"))?;
        }

        tokio::time::sleep(interval).await;
    }
}

#[allow(dead_code)]
pub async fn download_sample(url: Url) -> anyhow::Result<()> {
    let resp = reqwest::Client::new()
        .get(url.clone())
        .timeout(Duration::from_secs(60))
        .send()
        .await?
        .error_for_status()?;
    let body = resp.bytes().await?;
    if body.is_empty() {
        warn!("downloaded body from {url} was empty");
    }
    Ok(())
}

pub fn find_binary(name: &str) -> anyhow::Result<PathBuf> {
    let env_key = format!("CARGO_BIN_EXE_{}", name.replace('-', "_"));

    if let Ok(path) = std::env::var(env_key) {
        return Ok(PathBuf::from(path));
    }

    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./target"));

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let candidate = target_dir.join(profile).join(name);

    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(anyhow::anyhow!("could not locate binary {name} in {candidate:?}"))
    }
}
