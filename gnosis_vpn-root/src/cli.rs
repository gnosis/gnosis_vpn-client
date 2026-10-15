use clap::Parser;
use url::Url;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use gnosis_vpn_lib::dirs::{self, UserDirs};
use gnosis_vpn_lib::worker_params::{self, IdentityOverride, WorkerParams};
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

    /// State directory override for the pinned worker user; without it every user keeps state under their own home
    #[arg(long, env = dirs::ENV_VAR_STATE_HOME, requires = "worker_user")]
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

    /// Pin the worker to this user: every caller acts as it and the ownership gate is off (docker, tests);
    /// unset, the worker runs as whoever calls `start-client`
    #[arg(long, env = worker::ENV_VAR_WORKER_USER)]
    pub worker_user: Option<String>,

    /// Path to the worker binary
    #[arg(long, env = worker::ENV_VAR_WORKER_BINARY, default_value = worker::DEFAULT_WORKER_BINARY)]
    pub worker_binary: PathBuf,

    /// Hopr edge client configuration path
    #[arg(long, env = hopr::ENV_VAR_CONFIG, default_value = None) ]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr edge client identity path; one identity for everyone, so only with a pinned worker user
    #[arg(long, env = hopr::ENV_VAR_ID_FILE, default_value = None, requires = "worker_user")]
    pub hopr_identity_file: Option<PathBuf>,

    /// Hopr edge client identity pass; only with a pinned worker user
    #[arg(long, env = hopr::ENV_VAR_ID_PASS, default_value = None, requires = "worker_user")]
    pub hopr_identity_pass: Option<String>,

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

    /// Autostart client after service startup and shut it down after being idle for this duration.
    /// The idle countdown is suspended while a VPN connection is active and resumes on disconnect.
    /// Nobody asked for a worker at boot, so this needs a pinned worker user.
    #[arg(long, env = worker::ENV_VAR_CLIENT_AUTOSTART, default_value = None,
                value_parser = humantime::parse_duration, requires = "worker_user"
        )]
    pub client_autostart: Option<Duration>,
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

        let identity = IdentityOverride {
            file: self.hopr_identity_file.clone(),
            pass: self.hopr_identity_pass.clone(),
        };

        WorkerParams::new(
            identity,
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

    /// Each of these would hand every local user one shared identity or state dir.
    #[test]
    fn shared_state_flags_need_a_pinned_worker_user() {
        for flag in [
            ["--state-home", "/var/lib/gnosisvpn"],
            ["--hopr-identity-file", "/etc/gnosisvpn/hopr.id"],
            ["--hopr-identity-pass", "secret"],
            ["--client-autostart", "10m"],
        ] {
            let mut args = base_args();
            args.extend(flag);
            assert!(
                Cli::try_parse_from(args.clone()).is_err(),
                "{flag:?} accepted without --worker-user"
            );
            args.extend(["--worker-user", "gnosisvpn"]);
            assert!(
                Cli::try_parse_from(args).is_ok(),
                "{flag:?} rejected with --worker-user"
            );
        }
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
