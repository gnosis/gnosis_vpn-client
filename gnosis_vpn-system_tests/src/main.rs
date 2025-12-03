mod cli;
mod fixtures;

use anyhow::Result;
use clap::Parser;
use gnosis_vpn_lib::hopr::hopr_lib;
use rand::seq::IndexedRandom;
use std::time::Duration;
use tracing::{error, info};

use cli::{Cli, Command, DownloadArgs};
use fixtures::control_client::ControlClient;
use fixtures::lib;
use fixtures::service::Service;

fn main() {
    info!("starting gnosis_vpn system tests");
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
    Service::spawn(&gnosis_bin, &cli.shared, &socket_path)?;

    // wait for up to 30s for service to be running
    client.wait_for_service_running(Duration::from_secs(30)).await?;

    // wait for up to 60s for safe to be created
    client.wait_for_safe_created(Duration::from_secs(60)).await?;

    // wait for up to 30min for the node to be in Running state
    client.wait_for_node_running(Duration::from_secs(30 * 60)).await?;

    // wait for up to 30s for node to be funded (should be instant as already funded for now)
    client.wait_for_node_funding(Duration::from_secs(30)).await?;

    // wait for up to 2min for destination to be ready to be used
    let destinations = client.wait_for_ready_destinations(Duration::from_secs(2 * 60)).await?;

    // Pick a random destination that is connectable
    let destination = destinations
        .choose(&mut rand::rng())
        .expect("destinations should not be empty")
        .clone();

    info!(
        state = ?client.connect(destination.address).await?,
        "Initiated connection",
    );

    client
        .wait_for_connection_established(&destination, Duration::from_secs(180))
        .await?;

    // connect to a different destination and perform download tests
    // wait for up to 2min for destination to be ready to be used
    let destinations = client.wait_for_ready_destinations(Duration::from_secs(2 * 60)).await?;

    // Pick a random destination that is connectable
    let filtered_destinations = destinations
        .iter()
        .filter(|d| d.address != destination.address)
        .collect::<Vec<_>>();

    let destination = filtered_destinations
        .choose(&mut rand::rng())
        .expect("should have at least one different destination available");

    info!(
        state = ?client.connect(destination.address).await?,
        "Initiated connection",
    );

    client
        .wait_for_connection_established(destination, Duration::from_secs(180))
        .await?;

    // Query public IP trough the VPN
    let ip = lib::fetch_public_ip(&cli.shared.ip_echo_url, cli.shared.proxy.as_ref()).await?;
    info!(public_ip = %ip, "queried public IP via the echo service");

    match cli.command {
        Command::Download(args) => {
            download_files(&args, cli.shared.proxy.as_ref()).await?;
        }
    }

    client.wait_for_disconnection(Duration::from_secs(15)).await?;

    Ok(())
}

async fn download_files(args: &DownloadArgs, proxy: Option<&url::Url>) -> Result<()> {
    for idx in 0..args.attempts {
        let file_size = args.min_size_bytes * (args.size_factor.pow(idx) as u64);
        info!(%file_size, "performing sample download attempt #{}/{}", idx + 1, args.attempts);

        match lib::download_file(file_size, proxy).await {
            Ok(_) => info!(%file_size, "sample download succeeded"),
            Err(e) => {
                error!(%file_size, "sample download failed {e}");
                return Err(e);
            }
        }
    }

    Ok(())
}
