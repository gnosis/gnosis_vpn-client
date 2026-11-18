use backon::BackoffBuilder;
use edgli::blokli::{IncentiveOperations, make_incentive_operations};
use edgli::hopr_lib::HoprKeys;
use edgli::hopr_lib::config::HoprLibConfig;
use edgli::{BlokliDnsOverride, BlokliEndpoint};
use gnosis_vpn_ipc::StartupIssueKind;
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
use crate::dirs::{self, UserDirs};
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::{config, identity};
use crate::remote_data;

const TICKET_STORE_FILE: &str = "tickets.redb";

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
    #[error("identity file without pass file in {dir}")]
    PassFileMissing { dir: PathBuf },
}

/// What root knows at spawn; the worker learns the network from Blokli itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnParams {
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    allow_experimental: bool,
    allow_funding_tool_rerun: bool,
    blokli_url: Url,
    /// Set while root's killswitch blocks DNS: the address the previous worker used.
    pinned_blokli_ip: Option<Ipv4Addr>,
    user_dirs: UserDirs,
}

#[derive(Clone, Debug)]
pub struct WorkerParams {
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    allow_experimental: bool,
    allow_funding_tool_rerun: bool,
    blokli_url: Url,
    /// Fixed for edgli's lifetime; a move is only picked up by a new worker.
    blokli_ip: Ipv4Addr,
    /// The killswitch was armed when this worker spawned, so DNS stays blocked.
    spawned_armed: bool,
    user_dirs: UserDirs,
    /// Blokli's `chainInfo.network`; it keys identity, safe cache and PIX state.
    network: String,
}

#[derive(Debug, Error)]
#[error("blokli reported a network name unusable as a directory name: {0:?}")]
pub struct InvalidNetworkName(pub String);

#[derive(Debug, Error)]
enum NetworkLookupError {
    #[error(transparent)]
    RemoteData(#[from] remote_data::Error),
    #[error(transparent)]
    NetworkName(#[from] InvalidNetworkName),
}

/// Bounded, since a resolver behind a blocking firewall never answers.
async fn resolve_blokli_ips(url: &Url, timeout: Duration) -> Result<Vec<Ipv4Addr>, remote_data::Error> {
    tokio::time::timeout(timeout, remote_data::resolve_blokli_ips(url))
        .await
        .map_err(|_| remote_data::Error::ResolveTimeout)?
}

impl NetworkLookupError {
    fn startup_issue_kind(&self) -> StartupIssueKind {
        match self {
            NetworkLookupError::RemoteData(error) => error.startup_issue_kind(),
            NetworkLookupError::NetworkName(_) => StartupIssueKind::Incompatible,
        }
    }
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
    pub funding_tool_rerun: bool,
}

impl SpawnParams {
    pub fn new(
        config_mode: ConfigFileMode,
        allow: AllowFlags,
        blokli_url: Url,
        pinned_blokli_ip: Option<Ipv4Addr>,
        user_dirs: UserDirs,
    ) -> Self {
        Self {
            config_mode,
            allow_insecure: allow.insecure,
            allow_experimental: allow.experimental,
            allow_funding_tool_rerun: allow.funding_tool_rerun,
            blokli_url,
            pinned_blokli_ip,
            user_dirs,
        }
    }

    /// Retries until Blokli names a usable network; every failure goes to `on_failure` for status.
    pub async fn query_network(
        self,
        request_timeout: Duration,
        resolve_timeout: Duration,
        on_failure: impl Fn(StartupIssueKind, String),
    ) -> WorkerParams {
        let mut delays = remote_data::backoff_expo_long_delay().without_max_times().build();
        loop {
            let error = match self.try_network(request_timeout, resolve_timeout).await {
                Ok(params) => return params,
                Err(error) => error,
            };
            let delay = delays.next().unwrap_or(Duration::from_secs(60));
            tracing::warn!(%error, ?delay, blokli_url = %self.blokli_url, "blokli network not queryable - retrying");
            on_failure(error.startup_issue_kind(), error.to_string());
            tokio::time::sleep(delay).await;
        }
    }

    async fn try_network(
        &self,
        request_timeout: Duration,
        resolve_timeout: Duration,
    ) -> Result<WorkerParams, NetworkLookupError> {
        let blokli_ip = match self.pinned_blokli_ip {
            Some(pinned) => pinned,
            None => {
                let ips = resolve_blokli_ips(&self.blokli_url, resolve_timeout).await?;
                ips.first().copied().ok_or(remote_data::Error::NoIpv4)?
            }
        };
        let network = remote_data::query_blokli_network(&self.blokli_url, blokli_ip, request_timeout).await?;
        Ok(WorkerParams::new(self.clone(), blokli_ip, network)?)
    }
}

impl WorkerParams {
    /// The network name becomes a directory name under the user's home.
    pub fn new(spawn: SpawnParams, blokli_ip: Ipv4Addr, network: String) -> Result<Self, InvalidNetworkName> {
        if !valid_network_name(&network) {
            return Err(InvalidNetworkName(network));
        }
        let SpawnParams {
            config_mode,
            allow_insecure,
            allow_experimental,
            allow_funding_tool_rerun,
            blokli_url,
            pinned_blokli_ip,
            user_dirs,
        } = spawn;
        Ok(Self {
            config_mode,
            allow_insecure,
            allow_experimental,
            allow_funding_tool_rerun,
            blokli_url,
            blokli_ip,
            spawned_armed: pinned_blokli_ip.is_some(),
            user_dirs,
            network,
        })
    }

    /// The per-network state tree, created by the worker as itself before anything is written into it.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        dirs::ensure_private_dir(&self.user_dirs.data, &self.pix_state_dir())?;
        dirs::ensure_private_dir(&self.user_dirs.cache, &self.network_cache_dir())
    }

    pub async fn persist_identity_generation(&self) -> Result<HoprKeys, Error> {
        let identity_file = identity::file(&self.network_data_dir());
        let path = identity::pass_file(&self.network_data_dir());
        let identity_pass = match fs::read_to_string(&path).await {
            Ok(p) => {
                tracing::debug!(?path, "read identity pass from file");
                Ok(p)
            }
            // A half-migrated dir: generating a pass now would leave the key undecryptable for good.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && identity_file.exists() => {
                let dir = self.network_data_dir();
                tracing::error!(
                    ?dir,
                    "identity file present but pass file missing - refusing to generate a new pass"
                );
                Err(Error::PassFileMissing { dir })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(?path, "no identity pass file - generating one next to the identity");
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
        }?;

        identity::from_path(identity_file, identity_pass.clone()).map_err(Error::from)
    }

    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = identity::file(&self.network_data_dir());
        let identity_pass = self.identity_pass().await?;
        identity::from_path(identity_file, identity_pass).map_err(Error::from)
    }

    /// Also seals the PIX pool state: it already must never get lost, so it adds no new way to strand funds.
    pub async fn identity_pass(&self) -> Result<String, Error> {
        let path = identity::pass_file(&self.network_data_dir());
        fs::read_to_string(&path).await.map_err(|e| {
            tracing::error!(error = %e, ?path, "failed to read HOPR identity pass file");
            Error::IOFile { path, source: e }
        })
    }

    pub async fn to_config(
        &self,
        safe_module: &SafeModule,
        options: crate::connection::options::HoprConfigOptions,
    ) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            ConfigFileMode::Manual(path) => config::from_path(path).await.map_err(Error::from),
            ConfigFileMode::Generated => config::generate(safe_module, options).await.map_err(Error::from),
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

    pub fn allow_funding_tool_rerun(&self) -> bool {
        self.allow_funding_tool_rerun
    }

    pub fn blokli_url(&self) -> Url {
        self.blokli_url.clone()
    }

    /// The address edgli's clients are built with; root exempts it from the killswitch and routes it via the WAN.
    pub fn blokli_ip(&self) -> Ipv4Addr {
        self.blokli_ip
    }

    pub fn spawned_armed(&self) -> bool {
        self.spawned_armed
    }

    /// Only meaningful while DNS is open; a failed lookup keeps the current address.
    pub async fn blokli_moved(&self, resolve_timeout: Duration) -> bool {
        match resolve_blokli_ips(&self.blokli_url, resolve_timeout).await {
            Ok(ips) if ips.is_empty() || ips.contains(&self.blokli_ip) => false,
            Ok(ips) => {
                tracing::info!(old = %self.blokli_ip, new = ?ips, "blokli address changed");
                true
            }
            Err(error) => {
                tracing::warn!(%error, current = %self.blokli_ip, "blokli host not resolvable before connect - keeping the current address");
                false
            }
        }
    }

    /// The Blokli endpoint to talk to, including how its host is resolved and how long a single
    /// request to it may take.
    ///
    /// `request_timeout` comes from the configuration file, which is read by the worker, while
    /// [`WorkerParams`] is built from CLI arguments in the root process - hence a parameter
    /// rather than a stored field.
    pub fn blokli_endpoint(&self, request_timeout: Duration) -> BlokliEndpoint {
        // A `None` port keeps the endpoint URL's port, which is what the IP was resolved for.
        BlokliEndpoint::new(self.blokli_url())
            .with_request_timeout(request_timeout)
            .with_dns_override(BlokliDnsOverride::new(IpAddr::V4(self.blokli_ip), None))
    }

    pub fn network(&self) -> &str {
        &self.network
    }

    /// `<data>/networks/<network>/`: identity files, the ticket store and the PIX pool state.
    pub fn network_data_dir(&self) -> PathBuf {
        self.user_dirs.network_data(&self.network)
    }

    /// `<data>/networks/<network>/tickets.redb`: the outgoing ticket indices, which must survive restarts.
    pub fn ticket_store_file(&self) -> PathBuf {
        self.network_data_dir().join(TICKET_STORE_FILE)
    }

    /// `<cache>/networks/<network>/`: the safe cache, re-queried from chain when missing.
    pub fn network_cache_dir(&self) -> PathBuf {
        self.user_dirs.network_cache(&self.network)
    }

    pub fn pix_state_dir(&self) -> PathBuf {
        self.user_dirs.pix_state(&self.network)
    }
}

/// `join` does no sanitizing: `..` climbs out and an absolute name replaces the base.
fn valid_network_name(name: &str) -> bool {
    let mut components = std::path::Path::new(name).components();
    let single_plain = matches!(components.next(), Some(std::path::Component::Normal(_)));
    single_plain && components.next().is_none()
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

    const BLOKLI_IP: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

    fn spawn(blokli_url: Url, user_dirs: UserDirs) -> SpawnParams {
        SpawnParams::new(
            ConfigFileMode::Generated,
            AllowFlags::default(),
            blokli_url,
            Some(BLOKLI_IP),
            user_dirs,
        )
    }

    fn params(blokli_url: Url) -> WorkerParams {
        let user_dirs = UserDirs {
            data: PathBuf::from("/tmp/gnosisvpn"),
            cache: PathBuf::from("/tmp/gnosisvpn/cache"),
            logs: PathBuf::from("/tmp/gnosisvpn/logs"),
        };
        WorkerParams::new(spawn(blokli_url, user_dirs), BLOKLI_IP, "jura-dev".to_string()).unwrap()
    }

    fn url(raw: &str) -> Url {
        raw.parse().unwrap()
    }

    /// Stands in for the configured `[blokli] request_timeout` where its value does not matter.
    const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn blokli_endpoint_keeps_the_configured_url_and_pins_it_to_the_address() {
        let configured = url("https://blokli.example.com/");
        let endpoint = params(configured.clone()).blokli_endpoint(TEST_REQUEST_TIMEOUT);
        assert_eq!(endpoint.url, configured);
        // A `None` port leaves the endpoint URL's port in place.
        assert_eq!(
            endpoint.dns_override,
            Some(BlokliDnsOverride::new(IpAddr::V4(BLOKLI_IP), None))
        );
    }

    /// The DNS override rebuilds the endpoint; that seam once dropped the override and could drop the timeout.
    #[test]
    fn blokli_endpoint_carries_the_configured_request_timeout() {
        let endpoint = params(url("https://blokli.example.com/")).blokli_endpoint(Duration::from_secs(45));
        assert_eq!(endpoint.request_timeout, Duration::from_secs(45));
        assert!(endpoint.dns_override.is_some());
    }

    /// Root hands `SpawnParams` to the worker serialized, so the pin must survive that hop.
    #[test]
    fn the_pinned_address_survives_a_serde_roundtrip() {
        let spawn = spawn(
            url("https://blokli.example.com/"),
            dirs::user_dirs(std::path::Path::new("/tmp")),
        );

        let json = serde_json::to_string(&spawn).unwrap();
        let restored: SpawnParams = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.pinned_blokli_ip, Some(BLOKLI_IP));
    }

    #[test]
    fn a_pinned_spawn_starts_armed() {
        let params = params(url("https://blokli.example.com/"));

        assert!(params.spawned_armed());
        assert_eq!(params.blokli_ip(), BLOKLI_IP);
    }

    /// An IP literal host resolves without DNS, so these run offline.
    #[tokio::test]
    async fn blokli_moved_only_when_the_lookup_drops_the_current_address() {
        let same = params(url("https://203.0.113.7/"));
        let moved = params(url("https://198.51.100.7/"));

        assert!(!same.blokli_moved(TEST_REQUEST_TIMEOUT).await);
        assert!(moved.blokli_moved(TEST_REQUEST_TIMEOUT).await);
    }

    #[test]
    fn network_names_must_be_plain_directory_names() {
        let dirs = dirs::user_dirs(std::path::Path::new("/tmp"));
        for good in ["jura-dev", "piz-palu-dev", "Jura_Dev"] {
            let spawn = spawn(url("https://blokli.example.com/"), dirs.clone());
            assert!(
                WorkerParams::new(spawn, BLOKLI_IP, good.to_string()).is_ok(),
                "{good:?} refused"
            );
        }
        for bad in ["", ".", "..", "../jura", "jura/dev", "/etc"] {
            let spawn = spawn(url("https://blokli.example.com/"), dirs.clone());
            assert!(
                WorkerParams::new(spawn, BLOKLI_IP, bad.to_string()).is_err(),
                "{bad:?} accepted"
            );
        }
    }

    #[test]
    fn state_paths_are_keyed_by_network() {
        let params = params(url("https://blokli.example.com/"));
        assert_eq!(
            params.network_data_dir(),
            PathBuf::from("/tmp/gnosisvpn/networks/jura-dev")
        );
        assert_eq!(
            params.network_cache_dir(),
            PathBuf::from("/tmp/gnosisvpn/cache/networks/jura-dev")
        );
        assert_eq!(
            params.pix_state_dir(),
            PathBuf::from("/tmp/gnosisvpn/networks/jura-dev/pix")
        );
        assert_eq!(
            params.ticket_store_file(),
            PathBuf::from("/tmp/gnosisvpn/networks/jura-dev/tickets.redb")
        );
    }

    fn params_in(tmp: &std::path::Path) -> WorkerParams {
        let spawn = spawn(url("https://blokli.example.com/"), dirs::user_dirs(tmp));
        WorkerParams::new(spawn, BLOKLI_IP, "jura-dev".to_string()).unwrap()
    }

    #[tokio::test]
    async fn an_empty_network_dir_generates_a_fresh_identity_with_a_private_pass() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp dir");
        let params = params_in(tmp.path());
        params.ensure_dirs().expect("dirs");

        params.persist_identity_generation().await.expect("fresh identity");

        let pass = identity::pass_file(&params.network_data_dir());
        let mode = std::fs::metadata(&pass).expect("pass file").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(params.network_data_dir())
            .expect("data dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let id = identity::file(&params.network_data_dir());
        let id_mode = std::fs::metadata(&id).expect("id file").permissions().mode() & 0o777;
        assert_eq!(id_mode, 0o600);
    }

    #[tokio::test]
    async fn loading_an_identity_readable_by_others_restricts_it_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp dir");
        let params = params_in(tmp.path());
        params.ensure_dirs().expect("dirs");
        params.persist_identity_generation().await.expect("fresh identity");
        let id = identity::file(&params.network_data_dir());
        std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        params.calc_keys().await.expect("load identity");

        let mode = std::fs::metadata(&id).expect("id file").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_fresh_tree_is_private_from_our_own_root_down() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp dir");
        let params = params_in(tmp.path());
        params.ensure_dirs().expect("dirs");

        let data = &params.user_dirs.data;
        let cache = &params.user_dirs.cache;
        let ours = [
            data.clone(),
            data.join("networks"),
            params.network_data_dir(),
            params.pix_state_dir(),
            cache.clone(),
            cache.join("networks"),
            params.network_cache_dir(),
        ];
        for dir in ours {
            let mode = std::fs::metadata(&dir).expect("dir").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
    }

    /// A migrated `.id` without its `.pass` must not be silently replaced by a key nobody can open.
    #[tokio::test]
    async fn an_identity_without_a_pass_file_is_an_error_not_a_regeneration() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let params = params_in(tmp.path());
        params.ensure_dirs().expect("dirs");
        std::fs::write(identity::file(&params.network_data_dir()), b"{}").expect("stale id");

        let err = params.persist_identity_generation().await.expect_err("must refuse");

        assert!(matches!(err, Error::PassFileMissing { .. }), "{err}");
        assert!(!identity::pass_file(&params.network_data_dir()).exists());
    }
}
