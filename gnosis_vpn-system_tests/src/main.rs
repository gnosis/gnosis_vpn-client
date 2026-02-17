mod cli;
mod download;
mod fixtures;
mod report;
mod workflow;

use anyhow::Result;
use clap::Parser;
use gnosis_vpn_lib::hopr::hopr_lib;
use tracing::{error, info};

fn main() {
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => tracing_subscriber::filter::EnvFilter::new("info"),
    };

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_thread_ids(true)
        .with_thread_names(true)
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    let res = match hopr_lib::prepare_tokio_runtime(None, None) {
        Ok(rt) => rt.block_on(main_inner()),
        Err(error) => {
            error!("error preparing tokio runtime: {error}");
            std::process::exit(exitcode::IOERR);
        }
    };

    match res {
        Ok(_) => std::process::exit(exitcode::OK),
        Err(error) => {
            error!("error running system tests: {error}");
            std::process::exit(exitcode::SOFTWARE);
        }
    }
}

/// Entry point for the asynchronous system test workflow.
async fn main_inner() -> Result<()> {
    let cli = cli::Cli::parse();
    let wf = workflow::SystemTestWorkflow::new(cli).await?;

    wf.run().await
}
