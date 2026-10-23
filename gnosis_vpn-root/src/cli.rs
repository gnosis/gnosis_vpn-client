use clap::Parser;
use url::Url;

use std::net::Ipv4Addr;
use std::path::PathBuf;

use gnosis_vpn_lib::dirs::{self, UserDirs};
use gnosis_vpn_lib::worker_params::{self, WorkerParams};
use gnosis_vpn_lib::{config, hopr, logging, socket};

use crate::{ENV_VAR_PID_FILE, worker};

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// Socket path for communication with this service
    #[arg(
        short,
        long,
        env = socket::root::ENV_VAR,
        default_value = socket::root::DEFAULT_PATH
    )]
    pub socket_path: PathBuf,

    /// General configuration file
    #[arg(
        short,
        long,
        env = config::ENV_VAR,
        default_value = config::DEFAULT_PATH,
        )]
    pub config_path: PathBuf,

    /// State directory for every caller's worker instead of their own home; the calling user must be able to write it
    #[arg(long, env = dirs::ENV_VAR_STATE_HOME)]
    pub state_home: Option<PathBuf>,

    /// Log file path - provide if you want the service to manage the log file
    #[arg(
        long,
        env = logging::ENV_VAR_LOG_FILE,
    )]
    pub log_file: Option<PathBuf>,

    /// PID file path - needed if cannot get the process id otherwise
    #[arg(
        long,
        env = ENV_VAR_PID_FILE,
    )]
    pub pid_file: Option<PathBuf>,

    /// Path to the worker binary
    #[arg(long, env = worker::ENV_VAR_WORKER_BINARY, default_value = worker::DEFAULT_WORKER_BINARY)]
    pub worker_binary: PathBuf,

    /// Hopr edge client configuration path
    #[arg(long, env = hopr::ENV_VAR_CONFIG, default_value = None) ]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr Blokli URL used for on chain queries
    #[arg(long, env = hopr::ENV_VAR_BLOKLI_URL)]
    pub hopr_blokli_url: Url,

    /// Allow insecure non-private connections (only for testing purposes)
    #[arg(long)]
    pub allow_insecure: bool,

    /// Allow experimental multi-hop connections with 2+ hops (only for testing purposes)
    #[arg(long)]
    pub allow_experimental: bool,

    /// Allow re-running the funding tool even after a Safe has been deployed and the node is
    /// running, subject to a cooldown after each successful run (only for support/testing
    /// purposes)
    #[arg(long)]
    pub allow_funding_tool_rerun: bool,
}

pub fn parse() -> Cli {
    Cli::parse()
}

impl Cli {
    /// The worker's view of these arguments plus what root resolved at startup and who owns the worker.
    pub fn worker_params(&self, blokli_ip: Ipv4Addr, network: &str, user_dirs: UserDirs) -> WorkerParams {
        let config_mode = match self.hopr_config_path.clone() {
            Some(path) => worker_params::ConfigFileMode::Manual(path),
            None => worker_params::ConfigFileMode::Generated,
        };
        let allow = worker_params::AllowFlags {
            insecure: self.allow_insecure,
            experimental: self.allow_experimental,
            funding_tool_rerun: self.allow_funding_tool_rerun,
        };

        WorkerParams::new(
            config_mode,
            allow,
            self.hopr_blokli_url.clone(),
            blokli_ip,
            user_dirs,
            network.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> Vec<&'static str> {
        vec![
            "gnosis_vpn",
            "--socket-path",
            "/tmp/gnosis.socket",
            "--config-path",
            "/tmp/gnosis.toml",
            // required since edgli dropped its production default
            "--hopr-blokli-url",
            "https://blokli.example.com",
        ]
    }

    #[test]
    fn parses_cli_with_minimum_arguments() -> anyhow::Result<()> {
        let args = Cli::try_parse_from(base_args())?;
        assert!(args.hopr_config_path.is_none());

        Ok(())
    }

    /// There is deliberately no fallback Blokli endpoint: a deployment must say which one it talks to.
    #[test]
    fn rejects_cli_without_a_blokli_url() {
        let args = [
            "gnosis_vpn",
            "--socket-path",
            "/tmp/gnosis.socket",
            "--config-path",
            "/tmp/gnosis.toml",
        ];
        assert!(Cli::try_parse_from(args).is_err());
    }
}
