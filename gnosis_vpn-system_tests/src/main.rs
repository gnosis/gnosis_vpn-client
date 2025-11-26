mod cli;
mod fixtures;

use anyhow::Result;
use clap::Parser;
use gnosis_vpn_lib::hopr::hopr_lib;
use rand::seq::IndexedRandom;
use tracing::{error, info};

use cli::{Cli, Command, DownloadArgs};
use fixtures::control_client::ControlClient;
use fixtures::lib;
use fixtures::service_guard::ServiceGuard;

fn main() {
    let res = match hopr_lib::prepare_tokio_runtime() {
        Ok(rt) => rt.block_on(main_inner()),
        Err(e) => {
            error!("error preparing tokio runtime: {}", e);
            std::process::exit(exitcode::IOERR);
        }
    };

    match res {
        Ok(_) => std::process::exit(exitcode::OK),
        Err(e) => {
            error!("error running system tests: {}", e);
            std::process::exit(exitcode::SOFTWARE);
        }
    }
}

/// Entry point for the asynchronous system test workflow.
async fn main_inner() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    let cli = Cli::parse();

    let (gnosis_bin, socket_path) = match lib::prepare_configs().await {
        Ok(res) => res,
        Err(e) => return Err(e),
    };

    let client = ControlClient::new(socket_path.clone());
    let service = match ServiceGuard::spawn(&gnosis_bin, &cli.shared, &socket_path) {
        Ok(process) => process,
        Err(e) => return Err(e),
    };

    // wait for up to 30s for service to be running
    client.wait_for_service_running().await?;

    // wait for up to 30s for node to be funded (should be instant as already funded for now)
    client.wait_for_node_funding().await?;

    // wait for up to 30min for the node to be in Running state
    client.wait_for_node_running().await?;

    // wait for up to 2min for destination to be ready to be used
    let destinations = client.wait_for_ready_destinations().await?;

    // Pick a random destination that is connectable
    let destination = destinations
        .choose(&mut rand::rng())
        .expect("destinations should not be empty")
        .clone();

    let state = client.connect(destination.address).await?;
    info!("Connection state: {:?}", state);

    client.wait_for_connection_established(&destination).await?;

    // Query public IP
    let ip = lib::fetch_public_ip(&cli.shared.ip_echo_url, None).await?;
    info!(public_ip = %ip, "queried public IP via echo service");

    match cli.command {
        Command::Download(args) => {
            perform_download_attempts(&args, cli.shared.proxy.as_ref(), 5, true).await?;
        }
    }

    drop(service);

    Ok(())
}

async fn perform_download_attempts(
    args: &DownloadArgs,
    proxy: Option<&url::Url>,
    attempts: u32,
    fail_fast: bool,
) -> Result<()> {
    for idx in 0..attempts {
        let file_size = args.download_min_size_bytes * (2u64.pow(idx as u32));
        info!(%file_size, "performing sample download attempt #{}/{}", idx + 1, attempts);

        match lib::download_file(&args.download_url, file_size, proxy).await {
            Ok(_) => info!(%file_size, "sample download succeeded"),
            Err(e) => {
                error!(%file_size, "sample download failed {e}");
                if fail_fast {
                    return Err(e);
                }
            }
        }
    }

    Ok(())
}
