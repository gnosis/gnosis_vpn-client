use clap::Parser;
use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::hopr_params::{self, HoprParams};
use gnosis_vpn_lib::network::Network;
use gnosis_vpn_lib::{config, hopr, socket};

use crate::worker;

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// Socket path for communication with this servive
    #[arg(
        short,
        long,
        env = socket::ENV_VAR,
        default_value = socket::DEFAULT_PATH
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

    /// Username of the worker user (needs a home folder for caching and configurations)
    #[arg(long, env = worker::ENV_VAR_WORKER_USER, default_value = worker::DEFAULT_WORKER_USER)]
    pub worker_user: String,

    /// Path to the worker binary - relative to the users home folder
    #[arg(long, env = worker::ENV_VAR_WORKER_BINARY, default_value = worker::DEFAULT_WORKER_BINARY)]
    pub worker_binary: PathBuf,

    /// RPC provider URL needed for fat Hopr edge client
    #[arg(long, env = hopr::RPC_PROVIDER_ENV)]
    pub hopr_rpc_provider: Url,

    /// Hopr network
    #[arg(long, env = hopr::NETWORK_ENV, default_value = "dufour")]
    pub hopr_network: Network,

    /// Hopr edge client configuration path
    #[arg( long, env = hopr::CONFIG_ENV, default_value = None) ]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr edge client identity path
    #[arg( long, env = hopr::ID_FILE_ENV, default_value = None)]
    pub hopr_identity_file: Option<PathBuf>,

    /// Hopr edge client identity pass
    #[arg( long, env = hopr::ID_PASS_ENV, default_value = None)]
    pub hopr_identity_pass: Option<String>,

    /// Allow insecure non-private connections (only for testing purposes)
    #[arg(long)]
    pub allow_insecure: bool,
}

pub fn parse() -> Cli {
    Cli::parse()
}

impl From<Cli> for HoprParams {
    fn from(cli: Cli) -> Self {
        let network = cli.hopr_network;
        let rpc_provider = cli.hopr_rpc_provider;
        let config_mode = match cli.hopr_config_path {
            Some(path) => hopr_params::ConfigFileMode::Manual(path),
            None => hopr_params::ConfigFileMode::Generated,
        };
        let allow_insecure = cli.allow_insecure;

        HoprParams::new(
            cli.hopr_identity_file,
            cli.hopr_identity_pass,
            config_mode,
            network,
            rpc_provider,
            allow_insecure,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> Vec<&'static str> {
        vec![
            "gnosis_vpn",
            "--hopr-rpc-provider",
            "https://example.com",
            "--socket-path",
            "/tmp/gnosis.socket",
            "--config-path",
            "/tmp/gnosis.toml",
        ]
    }

    #[test]
    fn parses_cli_with_minimum_arguments() -> anyhow::Result<()> {
        let args = Cli::try_parse_from(base_args())?;
        assert_eq!(args.hopr_network, Network::Dufour);
        assert!(args.hopr_config_path.is_none());

        Ok(())
    }

    #[test]
    fn cli_parse_fails_when_rpc_provider_missing() -> anyhow::Result<()> {
        assert!(Cli::try_parse_from(["gnosis_vpn"]).is_err());

        Ok(())
    }

    #[test]
    fn hopr_params_conversion_preserves_network_and_security_flags() -> anyhow::Result<()> {
        let cli = Cli {
            socket_path: PathBuf::from("/tmp/socket"),
            config_path: PathBuf::from("/tmp/config"),
            hopr_rpc_provider: Url::parse("https://hopr.net").expect("url"),
            hopr_network: Network::Rotsee,
            hopr_config_path: Some(PathBuf::from("/tmp/hopr-config")),
            hopr_identity_file: Some(PathBuf::from("/tmp/id")),
            hopr_identity_pass: Some("secret-pass".into()),
            allow_insecure: true,
        };

        let params = HoprParams::from(cli.clone());
        assert_eq!(params.network(), cli.hopr_network);
        assert_eq!(params.rpc_provider(), cli.hopr_rpc_provider);
        assert!(params.allow_insecure());

        Ok(())
    }
}
