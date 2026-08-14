use edgli::blokli::{IncentiveOperations, make_incentive_operations};
use edgli::hopr_lib::HoprKeys;
use edgli::hopr_lib::config::HoprLibConfig;
use edgli::{BlokliDnsOverride, BlokliEndpoint};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::compat::SafeModule;
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::{self, config, identity};
use crate::remote_data;

#[derive(Debug, Error)]
pub enum Error {
    #[error("HOPR identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error accessing {path}: {source}")]
    IOFile { path: PathBuf, source: std::io::Error },
    #[error("HOPR config error: {0}")]
    Config(#[from] config::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Blokli creation error: {0}")]
    BlokliCreation(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerParams {
    identity_file: Option<PathBuf>,
    identity_pass: Option<String>,
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    allow_experimental: bool,
    allow_deployed_funding_tool: bool,
    blokli_url: Option<Url>,
    /// Address the Blokli host resolved to at service startup, see [`WorkerParams::resolve_blokli_ip`].
    resolved_blokli_ip: Option<Ipv4Addr>,
    state_home: PathBuf,
    cached_blokli_ips: Vec<Ipv4Addr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

/// Opt-in behaviors that are otherwise off by default (testing/support use only).
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowFlags {
    pub insecure: bool,
    pub experimental: bool,
    pub deployed_funding_tool: bool,
}

impl WorkerParams {
    pub fn new(
        identity_file: Option<PathBuf>,
        identity_pass: Option<String>,
        config_mode: ConfigFileMode,
        allow: AllowFlags,
        blokli_url: Option<Url>,
        state_home: PathBuf,
    ) -> Self {
        Self {
            identity_file,
            identity_pass,
            config_mode,
            allow_insecure: allow.insecure,
            allow_experimental: allow.experimental,
            allow_deployed_funding_tool: allow.deployed_funding_tool,
            blokli_url,
            resolved_blokli_ip: None,
            state_home,
            cached_blokli_ips: Vec::new(),
        }
    }

    pub fn set_cached_blokli_ips(&mut self, ips: Vec<Ipv4Addr>) {
        self.cached_blokli_ips = ips;
    }

    pub fn cached_blokli_ips(&self) -> &[Ipv4Addr] {
        &self.cached_blokli_ips
    }

    pub async fn persist_identity_generation(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => {
                tracing::info!(?path, "Using provided HOPR identity file");
                path.to_path_buf()
            }
            None => identity::file(self.state_home()),
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => {
                tracing::info!("Using provided HOPR identity pass");
                pass.to_string()
            }
            None => {
                let path = identity::pass_file(self.state_home());
                match fs::read_to_string(&path).await {
                    Ok(p) => {
                        tracing::debug!(?path, "No HOPR identity pass provided - read from file instead");
                        Ok(p)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::debug!(
                            ?path,
                            "No HOPR identity pass provided - generating new one and storing alongside identity file"
                        );
                        let pw = identity::generate_pass();
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&path)
                            .await
                            .map_err(|e| {
                                tracing::error!(error = %e, ?path, "failed to create HOPR identity pass file");
                                Error::IOFile {
                                    path: path.clone(),
                                    source: e,
                                }
                            })?;
                        file.write_all(pw.as_bytes()).await.map_err(|e| {
                            tracing::error!(error = %e, ?path, "failed to write generated HOPR identity pass file");
                            Error::IOFile {
                                path: path.clone(),
                                source: e,
                            }
                        })?;
                        Ok(pw)
                    }
                    Err(e) => {
                        tracing::error!(error = %e, ?path, "failed to read HOPR identity pass file");
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            log_path_diagnostics(&path);
                        }
                        Err(Error::IOFile { path, source: e })
                    }
                }?
            }
        };

        identity::from_path(identity_file, identity_pass.clone()).map_err(Error::from)
    }

    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => identity::file(self.state_home()),
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file(self.state_home());
                fs::read_to_string(&path).await.map_err(|e| {
                    tracing::error!(error = %e, ?path, "failed to read HOPR identity pass file");
                    Error::IOFile {
                        path: path.clone(),
                        source: e,
                    }
                })?
            }
        };

        identity::from_path(identity_file, identity_pass.clone()).map_err(Error::from)
    }

    pub async fn to_config(
        &self,
        safe_module: &SafeModule,
        path_planner_min_ack_rate: f64,
    ) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            ConfigFileMode::Manual(path) => config::from_path(path).await.map_err(Error::from),
            ConfigFileMode::Generated => config::generate(safe_module, path_planner_min_ack_rate)
                .await
                .map_err(Error::from),
        }
    }

    /// Create an [`IncentiveOperations`] handle for pre-Safe on-chain interactions.
    pub async fn create_incentive_operations(
        &self,
        config: BlokliConfig,
    ) -> Result<Arc<dyn IncentiveOperations>, Error> {
        let keys = self.calc_keys().await?;
        let private_key = keys.chain_key;
        let endpoint = self.blokli_endpoint(config.request_timeout);
        let ops = make_incentive_operations(endpoint, &private_key, Some(config.into()))
            .await
            .map_err(|e| Error::BlokliCreation(e.to_string()))?;
        Ok(Arc::from(ops))
    }

    pub fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }

    pub fn allow_experimental(&self) -> bool {
        self.allow_experimental
    }

    pub fn allow_deployed_funding_tool(&self) -> bool {
        self.allow_deployed_funding_tool
    }

    pub fn blokli_url(&self) -> Option<Url> {
        self.blokli_url.clone()
    }

    /// Resolves the Blokli host once, so later Blokli traffic needs no DNS lookup.
    ///
    /// Called at service startup while DNS is still reachable: an active killswitch blocks DNS
    /// for the rest of the session, which would otherwise leave the Blokli client unable to
    /// resolve its endpoint. Leaves the address unset on failure, falling back to system DNS.
    pub async fn resolve_blokli_ip(&mut self) {
        let url = hopr::blokli_url(self.blokli_url());
        match remote_data::resolve_ips(&url).await.map(|ips| ips.first().copied()) {
            Ok(Some(ip)) => {
                tracing::info!(%url, %ip, "resolved blokli host - pinning it for this session");
                self.resolved_blokli_ip = Some(ip);
            }
            Ok(None) => tracing::warn!(%url, "blokli host has no IPv4 address - falling back to system DNS"),
            Err(error) => {
                tracing::warn!(%url, %error, "failed to resolve blokli host - falling back to system DNS")
            }
        }
    }

    /// The address the Blokli host is pinned to, if it is known.
    ///
    /// Prefers the address resolved at startup and falls back to a cached one from an earlier
    /// connection, which covers a worker restart while the killswitch blocks DNS.
    pub fn pinned_blokli_ip(&self) -> Option<Ipv4Addr> {
        self.resolved_blokli_ip
            .or_else(|| self.cached_blokli_ips.first().copied())
    }

    /// The Blokli endpoint to talk to, including how its host is resolved and how long a single
    /// request to it may take.
    ///
    /// `request_timeout` comes from the configuration file, which is read by the worker, while
    /// [`WorkerParams`] is built from CLI arguments in the root process - hence a parameter
    /// rather than a stored field.
    pub fn blokli_endpoint(&self, request_timeout: Duration) -> BlokliEndpoint {
        let endpoint = BlokliEndpoint::new(hopr::blokli_url(self.blokli_url())).with_request_timeout(request_timeout);
        match self.pinned_blokli_ip() {
            // A `None` port keeps the endpoint URL's port, which is what the IP was resolved for.
            Some(ip) => endpoint.with_dns_override(BlokliDnsOverride::new(IpAddr::V4(ip), None)),
            None => endpoint,
        }
    }

    pub fn state_home(&self) -> PathBuf {
        self.state_home.clone()
    }
}

fn log_path_diagnostics(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(meta) => tracing::error!(
            uid = meta.uid(),
            gid = meta.gid(),
            mode = format!("{:o}", meta.mode() & 0o777),
            ?path,
            "pass file metadata"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::error!(?path, "pass file does not exist")
        }
        Err(e) => tracing::error!(error = %e, ?path, "pass file metadata error"),
    }
    if let Some(parent) = path.parent()
        && let Ok(meta) = std::fs::metadata(parent)
    {
        tracing::error!(
            uid = meta.uid(),
            gid = meta.gid(),
            mode = format!("{:o}", meta.mode() & 0o777),
            path = ?parent,
            "pass file parent directory metadata"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(blokli_url: Option<Url>) -> WorkerParams {
        WorkerParams::new(
            None,
            None,
            ConfigFileMode::Generated,
            AllowFlags::default(),
            blokli_url,
            PathBuf::from("/tmp/gnosisvpn"),
        )
    }

    fn url(raw: &str) -> Option<Url> {
        Some(raw.parse().unwrap())
    }

    /// Stands in for the configured `[blokli] request_timeout` in tests that do not care
    /// about its value.
    const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn blokli_endpoint_uses_system_dns_until_the_host_is_resolved() {
        let endpoint = params(None).blokli_endpoint(TEST_REQUEST_TIMEOUT);
        assert_eq!(endpoint.url, *edgli::DEFAULT_BLOKLI_URL);
        assert_eq!(endpoint.dns_override, None);
    }

    #[test]
    fn blokli_endpoint_keeps_configured_url() {
        let configured = url("https://blokli.example.com/").unwrap();
        let endpoint = params(Some(configured.clone())).blokli_endpoint(TEST_REQUEST_TIMEOUT);
        assert_eq!(endpoint.url, configured);
    }

    #[test]
    fn blokli_endpoint_carries_the_configured_request_timeout() {
        let endpoint = params(None).blokli_endpoint(Duration::from_secs(45));
        assert_eq!(endpoint.request_timeout, Duration::from_secs(45));
    }

    /// The DNS override is applied by rebuilding the endpoint, so the timeout has to survive
    /// that hop - the same seam that once dropped the DNS override itself.
    #[tokio::test]
    async fn a_pinned_host_keeps_the_configured_request_timeout() {
        let mut params = params(url("http://localhost:3002"));
        params.resolve_blokli_ip().await;

        let endpoint = params.blokli_endpoint(Duration::from_secs(45));
        assert_eq!(
            endpoint.dns_override,
            Some(BlokliDnsOverride::new(IpAddr::V4(Ipv4Addr::LOCALHOST), None))
        );
        assert_eq!(endpoint.request_timeout, Duration::from_secs(45));
    }

    #[tokio::test]
    async fn resolving_the_host_pins_the_endpoint_to_its_address() {
        let mut params = params(url("http://localhost:3002"));
        params.resolve_blokli_ip().await;

        assert_eq!(params.pinned_blokli_ip(), Some(Ipv4Addr::LOCALHOST));
        // A `None` port leaves the endpoint URL's port in place.
        assert_eq!(
            params.blokli_endpoint(TEST_REQUEST_TIMEOUT).dns_override,
            Some(BlokliDnsOverride::new(IpAddr::V4(Ipv4Addr::LOCALHOST), None))
        );
    }

    /// Blokli stays reachable via system DNS when startup resolution fails, rather than the
    /// service refusing to start.
    #[tokio::test]
    async fn an_unresolvable_host_leaves_the_endpoint_on_system_dns() {
        let mut params = params(url("file:///no-host-here"));
        params.resolve_blokli_ip().await;

        assert_eq!(params.pinned_blokli_ip(), None);
        assert_eq!(params.blokli_endpoint(TEST_REQUEST_TIMEOUT).dns_override, None);
    }

    /// A worker restarting while the killswitch blocks DNS gets no startup resolution, so the
    /// IP cached during the previous connection - the one the killswitch exempts - pins the host.
    #[test]
    fn a_cached_ip_pins_the_endpoint_when_startup_resolution_produced_nothing() {
        let mut params = params(None);
        params.set_cached_blokli_ips(vec![Ipv4Addr::new(203, 0, 113, 7), Ipv4Addr::new(203, 0, 113, 8)]);

        assert_eq!(params.pinned_blokli_ip(), Some(Ipv4Addr::new(203, 0, 113, 7)));
    }

    #[tokio::test]
    async fn the_startup_address_wins_over_a_cached_ip() {
        let mut params = params(url("http://localhost:3002"));
        params.resolve_blokli_ip().await;
        params.set_cached_blokli_ips(vec![Ipv4Addr::new(203, 0, 113, 7)]);

        assert_eq!(params.pinned_blokli_ip(), Some(Ipv4Addr::LOCALHOST));
    }

    /// `WorkerParams` is serialized to hand it from the root process to the worker, so the
    /// address resolved on the root side has to survive that hop.
    #[tokio::test]
    async fn the_pinned_address_survives_a_serde_roundtrip() {
        let mut params = params(url("http://localhost:3002"));
        params.resolve_blokli_ip().await;

        let json = serde_json::to_string(&params).unwrap();
        let restored: WorkerParams = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.pinned_blokli_ip(), Some(Ipv4Addr::LOCALHOST));
    }
}
