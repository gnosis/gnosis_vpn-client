use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use url::{Url, form_urlencoded::Serializer};

const BASE_DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down";

/// Repeatedly evaluates `check` until it yields a value or the timeout expires.
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

        tokio::time::sleep(interval.min(timeout - start.elapsed())).await;
    }
}

/// Downloads a file of the provided size, optionally routing traffic through a proxy.
pub async fn download_file(size_bytes: u64, proxy: Option<&Url>) -> anyhow::Result<()> {
    let mut download_url = Url::parse(BASE_DOWNLOAD_URL)?;

    let existing_pairs = download_url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();

    let size_param = size_bytes.to_string();
    let mut serializer = Serializer::new(String::new());
    for (k, v) in existing_pairs {
        if k == "bytes" {
            continue;
        }
        serializer.append_pair(&k, &v);
    }
    serializer.append_pair("bytes", &size_param);
    let query = serializer.finish();
    download_url.set_query(Some(&query));

    let mut client = reqwest::Client::builder().timeout(Duration::from_secs(60));
    if let Some(proxy_url) = proxy {
        client = client.proxy(reqwest::Proxy::all(proxy_url.as_str())?);
        info!(url = %download_url, size_bytes, "downloading random file through proxy");
    } else {
        info!(url = %download_url, size_bytes, "downloading random file");
    }

    let resp = client
        .build()?
        .get(download_url.clone())
        .send()
        .await?
        .error_for_status()?;

    let body = resp.bytes().await?;

    if body.is_empty() {
        warn!("downloaded body from {download_url} was empty");
    } else if body.len() as u64 != size_bytes {
        warn!(
            expected = size_bytes,
            actual = body.len(),
            "downloaded body had unexpected size"
        );
    }
    Ok(())
}

/// Queries an IP echo endpoint (e.g. api.ipify.org) and returns the IP string.
pub async fn fetch_public_ip(ip_echo_url: &Url, proxy: Option<&Url>) -> anyhow::Result<String> {
    let mut client = reqwest::Client::builder().timeout(Duration::from_secs(30));
    if let Some(proxy_url) = proxy {
        client = client.proxy(reqwest::Proxy::all(proxy_url.as_str())?);
    }

    let response_text = client
        .build()?
        .get(ip_echo_url.clone())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let trimmed = response_text.trim().to_string();
    if trimmed.is_empty() {
        warn!(url = %ip_echo_url, "ip echo response was empty");
    }
    Ok(trimmed)
}

/// Attempts to resolve the a binary path for the current build profile.
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
        return Ok(candidate);
    }

    // When running via `nix build`/`nix run` the binaries are available under `result/bin`.
    let result_candidate = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join("result").join("bin").join(name));

    if let Some(path) = result_candidate {
        if path.exists() {
            return Ok(path);
        }
    }

    // Finally, search PATH so the Nix-installed binary can be picked up when it's symlinked globally.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':').map(PathBuf::from) {
            let path_candidate = dir.join(name);
            if path_candidate.exists() {
                return Ok(path_candidate);
            }
        }
    }

    Err(anyhow::anyhow!(
        "could not locate binary {name} in {candidate:?}, result/bin, or PATH"
    ))
}

/// Resolves the test configuration, binary path, and socket location on disk.
pub async fn prepare_configs() -> anyhow::Result<(PathBuf, PathBuf)> {
    let gnosis_bin = find_binary("gnosis_vpn")
        .with_context(|| "Build the gnosis_vpn binary first, e.g. `cargo build -p gnosis_vpn`")?;

    let working_dir = std::env::current_dir()?.join("tmp");
    let socket_path = working_dir.join("gnosis_vpn.sock");

    std::fs::create_dir_all(&working_dir)?;

    Ok((gnosis_bin, socket_path))
}
