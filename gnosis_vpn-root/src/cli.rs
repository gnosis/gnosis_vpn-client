use clap::Parser;
use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::worker_params::{self, WorkerParams};
use gnosis_vpn_lib::{config, dirs, hopr, logging, socket};

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

    #[arg(
        long,
        env = dirs::ENV_VAR_STATE_HOME,
        default_value = dirs::DEFAULT_STATE_HOME,
    )]
    pub state_home: PathBuf,

    #[arg(
        long,
        env = logging::ENV_VAR_LOG_FILE,
    )]
    pub log_file: Option<PathBuf>,

    #[arg(
        long,
        env = ENV_VAR_PID_FILE,
    )]
    pub pid_file: Option<PathBuf>,

    /// Username of the worker user (needs a home folder for caching and configurations)
    #[arg(long, env = worker::ENV_VAR_WORKER_USER, default_value = worker::DEFAULT_WORKER_USER)]
    pub worker_user: String,

    /// Path to the worker binary - relative to the users home folder
    #[arg(long, env = worker::ENV_VAR_WORKER_BINARY, default_value = worker::DEFAULT_WORKER_BINARY)]
    pub worker_binary: PathBuf,

    /// Hopr edge client configuration path
    #[arg( long, env = hopr::ENV_VAR_CONFIG, default_value = None) ]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr edge client identity path
    #[arg( long, env = hopr::ENV_VAR_ID_FILE, default_value = None)]
    pub hopr_identity_file: Option<PathBuf>,

    /// Hopr edge client identity pass
    #[arg( long, env = hopr::ENV_VAR_ID_PASS, default_value = None)]
    pub hopr_identity_pass: Option<String>,

    /// Override internal Hopr Blokli URL used for on chain queries
    #[arg( long, env = hopr::ENV_VAR_BLOKLI_URL, default_value = None)]
    pub hopr_blokli_url: Option<Url>,

    /// Allow insecure non-private connections (only for testing purposes)
    #[arg(long)]
    pub allow_insecure: bool,

    /// Avoid dynamic peer discovery while connected to the VPN
    #[arg(long, env = worker::ENV_VAR_FORCE_STATIC_ROUTING)]
    pub force_static_routing: bool,
}

pub fn parse() -> Cli {
    Cli::parse()
}

impl From<&Cli> for WorkerParams {
    fn from(cli: &Cli) -> Self {
        let config_mode = match cli.hopr_config_path.clone() {
            Some(path) => worker_params::ConfigFileMode::Manual(path),
            None => worker_params::ConfigFileMode::Generated,
        };
        let allow_insecure = cli.allow_insecure;
        let force_static_routing = cli.force_static_routing;
        let state_home = cli.state_home.clone();

        WorkerParams::new(
            cli.hopr_identity_file.clone(),
            cli.hopr_identity_pass.clone(),
            config_mode,
            allow_insecure,
            cli.hopr_blokli_url.clone(),
            force_static_routing,
            state_home,
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
        ]
    }

    #[test]
    fn parses_cli_with_minimum_arguments() -> anyhow::Result<()> {
        let args = Cli::try_parse_from(base_args())?;
        assert!(args.hopr_config_path.is_none());

        Ok(())
    }
}
