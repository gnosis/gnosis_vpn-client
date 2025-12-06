use clap::Parser;

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {}

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
