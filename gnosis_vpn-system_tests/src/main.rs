mod fixtures;

use anyhow::Context;

use gnosis_vpn_lib::{
    command::{BalanceResponse, RunMode},
    connection::{destination::Destination, destination_health::Health},
    hopr::hopr_lib,
};
use rand::seq::IndexedRandom;
use std::{path::PathBuf, process, time::Duration};
use tracing::{error, info, warn};

use fixtures::control_client::ControlClient;
use fixtures::lib;
use fixtures::service_guard::ServiceGuard;
use fixtures::system_test_config::SystemTestConfig;

async fn prepare_configs() -> anyhow::Result<(SystemTestConfig, PathBuf, PathBuf)> {
    let cfg = match SystemTestConfig::load().await {
        Ok(Some(config)) => config,
        Ok(None) => {
            return Err(anyhow::anyhow!("no system test config found, skipping system tests"));
        }
        Err(e) => return Err(e),
    };

    let gnosis_bin = lib::find_binary("gnosis_vpn")
        .with_context(|| "Build the gnosis_vpn binary first, e.g. `cargo build -p gnosis_vpn`")?;

    let working_dir = match std::env::current_dir() {
        Ok(dir) => PathBuf::from(dir).join("tmp"),
        Err(e) => {
            error!("error getting current directory: {}", e);
            process::exit(exitcode::IOERR);
        }
    };

    match std::fs::create_dir_all(&working_dir) {
        Ok(_) => {}
        Err(e) => {
            error!("error creating working directory {:?}: {}", working_dir, e);
            process::exit(exitcode::IOERR);
        }
    };

    let socket_path = working_dir.join("gnosis_vpn.sock");

    info!("Using socket path at {:?}", socket_path);

    Ok((cfg, gnosis_bin, socket_path))
}

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

async fn main_inner() {
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    let (cfg, gnosis_bin, socket_path) = match prepare_configs().await {
        Ok(res) => res,
        Err(e) => {
            error!("error preparing system test config: {}", e);
            process::exit(exitcode::CONFIG);
        }
    };

    let service = match ServiceGuard::spawn(&gnosis_bin, &cfg, &socket_path) {
        Ok(process) => process,
        Err(e) => {
            error!("error spawning gnosis_vpn service: {}", e);
            process::exit(exitcode::SOFTWARE);
        }
    };
    let client = ControlClient::new(socket_path.clone());

    let _ = lib::wait_for_condition(
        "service running",
        Duration::from_secs(60),
        Duration::from_secs(2),
        || async {
            match client.ping().await {
                Ok(_) => {
                    info!("gnosis_vpn service is pingable");
                    Ok(Some(()))
                }
                Err(_) => Ok(None),
            }
        },
    )
    .await;

    let _ = lib::wait_for_condition(
        "node funds",
        Duration::from_secs(60 * 5),
        Duration::from_secs(5),
        || async {
            match client.balance().await {
                Ok(Some(BalanceResponse { node, safe, .. })) => {
                    if node.is_zero() {
                        return Err(anyhow::anyhow!("node has zero xDai balance, cannot proceed with test"));
                    }
                    if safe.is_zero() {
                        return Err(anyhow::anyhow!("safe has zero HOPR balance, cannot proceed with test"));
                    }
                    Ok(Some(()))
                }
                Ok(None) => Ok(None),
                Err(_) => Ok(None),
            }
        },
    )
    .await;

    // wait for up to 30min for the node to be in Running state
    let _ = lib::wait_for_condition(
        "node running",
        Duration::from_secs(60 * 30),
        Duration::from_secs(10),
        || async {
            match client.status().await {
                Ok(Some(status)) => {
                    if matches!(status.run_mode, RunMode::Running { .. }) {
                        info!("node is in Running state");
                        Ok(Some(status))
                    } else {
                        Err(anyhow::anyhow!("node not running yet"))
                    }
                }
                Ok(None) => Ok(None),
                Err(_) => Ok(None),
            }
        },
    )
    .await;

    // wait for up to 30min for the node to be in Running state
    let res = lib::wait_for_condition(
        "node running",
        Duration::from_secs(60 * 30),
        Duration::from_secs(10),
        || async {
            match client.status().await {
                Ok(Some(status)) => {
                    let ready_dests = status
                        .destinations
                        .iter()
                        .filter_map(|dest| {
                            dest.health.as_ref().and_then(|health| {
                                if health.health == Health::ReadyToConnect {
                                    Some(dest.destination.clone())
                                } else {
                                    None
                                }
                            })
                        })
                        .collect::<Vec<Destination>>();

                    if !ready_dests.is_empty() {
                        Ok(Some(ready_dests))
                    } else {
                        warn!("didn't find any destinations ready to connect yet");
                        Ok(None)
                    }
                }
                Ok(None) => Ok(None),
                Err(_) => Ok(None),
            }
        },
    )
    .await;

    let destinations = match res {
        Ok(dests) => {
            info!("destinations are: {:?}", dests);
            dests
        }
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
        Ok(state) => {
            info!("Connection state: {:?}", state);
        }
        Err(e) => {
            error!("error connecting to destination {}: {}", destination, e);
            process::exit(exitcode::DATAERR);
        }
    }

    // match lib::download_sample(cfg.download_url.clone()).await {
    //     Ok(_) => {
    //         info!("sample download succeeded");
    //     }
    //     Err(e) => {
    //         error!("sample download failed: {}", e);
    //     }
    // }

    // Keep the handle alive until the end of the test and drop it for cleanup.
    drop(service);
}
