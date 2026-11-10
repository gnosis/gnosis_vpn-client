use clap::{Args, Parser, Subcommand};
use url::Url;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub shared: SharedArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Download(DownloadArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SharedArgs {
    /// Blokli URL used by the system tests.
    #[arg(
        long = "blokliUrl",
        env = "SYSTEM_TEST_BLOKLI_URL",
        value_name = "URL",
        default_value = "https://blokli.piz-palu.dev.gnosisvpn.io/"
    )]
    pub blokli_url: Url,

    /// Allow insecure connections (e.g., self-signed certificates).
    #[arg(long = "allowInsecure", env = "SYSTEM_TEST_ALLOW_INSECURE", default_value_t = false)]
    pub allow_insecure: bool,

    /// Allow experimental multi-hop connections with 2+ hops.
    #[arg(
        long = "allowExperimental",
        env = "SYSTEM_TEST_ALLOW_EXPERIMENTAL",
        default_value_t = false
    )]
    pub allow_experimental: bool,

    /// The user the worker runs as; root refuses to start a worker for root itself.
    #[arg(
        long = "workerUser",
        env = "SYSTEM_TEST_WORKER_USER",
        value_name = "NAME",
        default_value = "gvpn-test"
    )]
    pub worker_user: String,
<<<<<<< HEAD
=======

    /// Exits the network must discover, with their expected countries; discovery checks are skipped without it.
    #[arg(long = "expectedExits", env = "SYSTEM_TEST_EXPECTED_EXITS", value_name = "PATH")]
    pub expected_exits: Option<PathBuf>,

    /// Exits sell PIX chunks; the connection must pay for one.
    #[arg(long = "expectPix", env = "SYSTEM_TEST_EXPECT_PIX", default_value_t = false)]
    pub expect_pix: bool,
>>>>>>> ca3f297 (test(system-tests): require a paid PIX chunk on networks with a pix_strategy (GNO-1072) (#1002))
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
