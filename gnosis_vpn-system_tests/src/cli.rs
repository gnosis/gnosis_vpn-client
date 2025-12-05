use clap::{Args, Parser, Subcommand};
use url::Url;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub shared: SharedArgs,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Download(DownloadArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SharedArgs {
    /// RPC endpoint used by the system tests.
    #[arg(
        long = "rpcProvider",
        env = "SYSTEM_TEST_RPC_PROVIDER",
        value_name = "URL",
        default_value = "https://gnosis-rpc.publicnode.com"
    )]
    pub rpc_provider: Url,

    /// Public IP echo endpoint leveraged to verify outbound connectivity.
    #[arg(
        long = "ipEchoUrl",
        env = "SYSTEM_TEST_IP_ECHO_URL",
        value_name = "URL",
        default_value = "https://api.ipify.org"
    )]
    pub ip_echo_url: Url,

    /// Network to use for the system tests.
    #[arg(
        long = "network",
        env = "SYSTEM_TEST_NETWORK",
        value_name = "NETWORK",
        default_value = "rotsee"
    )]
    pub network: String,

    /// Optional HTTP proxy used for download/upload requests.
    #[arg(
        long = "proxy",
        env = "SYSTEM_TEST_PROXY",
        value_name = "URL",
        default_value = "http://10.128.0.1:3128"
    )]
    pub proxy: Option<Url>,

    /// Allow insecure connections (e.g., self-signed certificates).
    #[arg(long = "allowInsecure", env = "SYSTEM_TEST_ALLOW_INSECURE", default_value_t = false)]
    pub allow_insecure: bool,
}

#[derive(Debug, Clone, Copy, Args)]
pub struct DownloadArgs {
    /// Minimum download size in bytes used for the connectivity check.
    #[arg(
        long = "minSizeBytes",
        env = "SYSTEM_TEST_DOWNLOAD_MIN_SIZE_BYTES",
        value_name = "SIZE_IN_BYTES",
        default_value = "16000"
    )]
    pub min_size_bytes: u64,

    /// Number of download attempts, each time multiplying the download size by size_factor
    #[arg(
        long = "attempts",
        env = "SYSTEM_TEST_DOWNLOAD_ATTEMPTS",
        value_name = "ATTEMPTS",
        default_value = "5"
    )]
    pub attempts: u32,

    /// Factor to multiply the download size after each attempt.
    #[arg(
        long = "sizeFactor",
        env = "SYSTEM_TEST_DOWNLOAD_SIZE_FACTOR",
        value_name = "SIZE_FACTOR",
        default_value = "2"
    )]
    pub size_factor: u32,

    /// Number of repetitions per file size when measuring download performance.
    #[arg(
        long = "downloadRepetitions",
        env = "SYSTEM_TEST_DOWNLOAD_REPETITIONS",
        value_name = "REPETITIONS",
        default_value = "5"
    )]
    pub repetitions: usize,
}
