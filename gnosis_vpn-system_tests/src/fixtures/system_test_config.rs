use anyhow::Context;
use url::Url;

use crate::fixtures::lib::env_string_with_default;

// Environment variable names for system test configuration
struct EnvVar {
    name: &'static str,
    default: &'static str,
}

impl EnvVar {
    const fn new(name: &'static str, default: &'static str) -> Self {
        Self { name, default }
    }
}

const RPC_PROVIDER_ENV: EnvVar = EnvVar::new("SYSTEM_TEST_RPC_PROVIDER", "https://gnosis-rpc.publicnode.com");
const DOWNLOAD_URL_ENV: EnvVar = EnvVar::new("SYSTEM_TEST_DOWNLOAD_URL", "https://speed.cloudflare.com/__down");
const DOWNLOAD_SIZE_BYTES_ENV: EnvVar = EnvVar::new("SYSTEM_TEST_DOWNLOAD_SIZE_BYTES", "16000");
const DOWNLOAD_PROXY_ENV: EnvVar = EnvVar::new("SYSTEM_TEST_DOWNLOAD_PROXY", "http://10.128.0.1:3128");
const IP_ECHO_URL_ENV: EnvVar = EnvVar::new("SYSTEM_TEST_IP_ECHO_URL", "https://api.ipify.org");

pub struct SystemTestConfig {
    pub rpc_provider: Url,
    pub download_url: Url,
    pub download_size_bytes: u64,
    pub download_proxy: Option<Url>,
    pub ip_echo_url: Url,
    pub allow_insecure: bool,
}

impl SystemTestConfig {
    pub async fn load() -> anyhow::Result<Option<Self>> {
        let rpc_provider = env_string_with_default(RPC_PROVIDER_ENV.name, RPC_PROVIDER_ENV.default);
        let download: String = env_string_with_default(DOWNLOAD_URL_ENV.name, DOWNLOAD_URL_ENV.default);
        let download_size_bytes: u64 =
            env_string_with_default(DOWNLOAD_SIZE_BYTES_ENV.name, DOWNLOAD_SIZE_BYTES_ENV.default)
                .parse()
                .context("invalid download size in bytes")?;
        let download_proxy = env_string_with_default(DOWNLOAD_PROXY_ENV.name, DOWNLOAD_PROXY_ENV.default);
        let ip_echo_url = env_string_with_default(IP_ECHO_URL_ENV.name, IP_ECHO_URL_ENV.default);

        let rpc_provider = Url::parse(&rpc_provider).context("invalid rpc provider url")?;
        let download_url = Url::parse(&download).context("invalid download url")?;
        let ip_echo_url = Url::parse(&ip_echo_url).context("invalid ip echo url")?;
        let download_proxy = if download_proxy.trim().is_empty() {
            None
        } else {
            Some(Url::parse(&download_proxy).context("invalid download proxy url")?)
        };

        Ok(Some(Self {
            rpc_provider,
            download_url,
            download_size_bytes,
            download_proxy,
            ip_echo_url,
            allow_insecure: true,
        }))
    }
}
