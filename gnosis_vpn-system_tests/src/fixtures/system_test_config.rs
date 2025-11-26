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
const DOWNLOAD_URL_ENV: EnvVar = EnvVar::new(
    "SYSTEM_TEST_DOWNLOAD_URL",
    "https://speed.cloudflare.com/__down?bytes=1048576",
);

pub struct SystemTestConfig {
    pub rpc_provider: Url,
    #[allow(dead_code)]
    pub download_url: Url,
    pub allow_insecure: bool,
}

impl SystemTestConfig {
    pub async fn load() -> anyhow::Result<Option<Self>> {
        let rpc_provider = env_string_with_default(RPC_PROVIDER_ENV.name, RPC_PROVIDER_ENV.default);
        let download: String = env_string_with_default(DOWNLOAD_URL_ENV.name, DOWNLOAD_URL_ENV.default);

        let rpc_provider = Url::parse(&rpc_provider).context("invalid rpc provider url")?;
        let download_url = Url::parse(&download).context("invalid download url")?;

        Ok(Some(Self {
            rpc_provider,
            download_url,
            allow_insecure: true,
        }))
    }
}
