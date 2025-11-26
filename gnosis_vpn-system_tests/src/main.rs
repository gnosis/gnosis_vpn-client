mod cli;
mod fixtures;

use std::process;

use anyhow::Result;
use clap::Parser;
use gnosis_vpn_lib::hopr::hopr_lib;
use rand::seq::IndexedRandom;
use tracing::{error, info};

use cli::{Cli, Command, DownloadArgs};
use fixtures::control_client::ControlClient;
use fixtures::lib;
use fixtures::service_guard::ServiceGuard;
use url::Url;

fn main() {
    match hopr_lib::prepare_tokio_runtime() {
        Ok(rt) => {
            rt.block_on(main_inner());
        }
        Err(e) => {
            error!("error preparing tokio runtime: {}", e);
            process::exit(exitcode::IOERR);
        }
    }
}

/// Entry point for the asynchronous system test workflow.
async fn main_inner() {
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    let cli = Cli::parse();

    let (gnosis_bin, socket_path) = match lib::prepare_configs().await {
        Ok(res) => res,
        Err(e) => {
            error!("error preparing system test config: {}", e);
            process::exit(exitcode::SOFTWARE);
        }
    };

    let client = ControlClient::new(socket_path.clone());
    let service = match ServiceGuard::spawn(&gnosis_bin, &cli.shared, &socket_path) {
        Ok(process) => process,
        Err(e) => {
            error!("error spawning gnosis_vpn service: {}", e);
            process::exit(exitcode::SOFTWARE);
        }
    };

    // wait for up to 30s for service to be running
    if let Err(e) = client.wait_for_service_running().await {
        error!("error while waiting for service to start: {}", e);
        process::exit(exitcode::SOFTWARE);
    }

    // wait for up to 30s for node to be funded (should be instant as already funded for now)
    if let Err(e) = client.wait_for_node_funding().await {
        error!("error while waiting for node funds: {}", e);
        process::exit(exitcode::DATAERR);
    }

    // wait for up to 30min for the node to be in Running state
    if let Err(e) = client.wait_for_node_running().await {
        error!("error while waiting for node to run: {}", e);
        process::exit(exitcode::DATAERR);
    }
    // wait for up to 2min for destination to be ready to be used
    let destinations = match client.wait_for_ready_destinations().await {
        Ok(dests) => dests,
        Err(e) => {
            error!("error getting ready to connect destinations: {}", e);
            process::exit(exitcode::DATAERR);
        }
    };

    // Pick a random destination that is connectable
    let destination = destinations
        .choose(&mut rand::rng())
        .expect("destinations should not be empty")
        .clone();

    match client.connect(destination.address).await {
        Ok(state) => info!("Connection state: {:?}", state),
        Err(e) => {
            error!("error connecting to destination {}: {}", destination, e);
            process::exit(exitcode::DATAERR);
        }
    }

    if let Err(e) = client.wait_for_connection_established(&destination).await {
        error!("error while waiting for connection establishment: {}", e);
        process::exit(exitcode::DATAERR);
    }

    // Query public IP
    match lib::fetch_public_ip(&cli.shared.ip_echo_url, None).await {
        Ok(ip) => info!(public_ip = %ip, "queried public IP via echo service"),
        Err(e) => error!("failed to fetch public IP: {}", e),
    }

    match cli.command {
        Command::Download(args) => {
            if let Err(e) = perform_download_attempts(&args, cli.shared.proxy.as_ref(), true).await {
                error!("download command failed: {e}");
                process::exit(exitcode::IOERR);
            }
        }
    }

    drop(service);
}

async fn perform_download_attempts(args: &DownloadArgs, proxy: Option<&Url>, fail_fast: bool) -> Result<()> {
    let attempts = 5;
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
