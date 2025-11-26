use std::{path::PathBuf, time::Duration};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use url::{Url, form_urlencoded::Serializer};

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

pub async fn download_random_file(base_url: &Url, size_bytes: u64, proxy: Option<&Url>) -> anyhow::Result<()> {
    let mut download_url = base_url.clone();
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
