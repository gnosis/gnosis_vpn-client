use clap::Parser;

use std::path::PathBuf;

use gnosis_vpn_lib::hopr_params::{self, HoprParams};
use gnosis_vpn_lib::{config, hopr, socket};

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

    // TODO: add blokli instance override here?
    /// HERE

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
        let config_mode = match cli.hopr_config_path {
            Some(path) => hopr_params::ConfigFileMode::Manual(path),
            None => hopr_params::ConfigFileMode::Generated,
        };
        let allow_insecure = cli.allow_insecure;

        HoprParams::new(
            cli.hopr_identity_file,
            cli.hopr_identity_pass,
            config_mode,
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

    #[test]
    fn hopr_params_conversion_preserves_and_security_flags() -> anyhow::Result<()> {
        let cli = Cli {
            socket_path: PathBuf::from("/tmp/socket"),
            config_path: PathBuf::from("/tmp/config"),
            hopr_config_path: Some(PathBuf::from("/tmp/hopr-config")),
            hopr_identity_file: Some(PathBuf::from("/tmp/id")),
            hopr_identity_pass: Some("secret-pass".into()),
            allow_insecure: true,
        };

        let params = HoprParams::from(cli.clone());
        assert!(params.allow_insecure());

        Ok(())
    }
}
