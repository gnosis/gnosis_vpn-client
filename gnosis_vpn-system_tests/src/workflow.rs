use anyhow::{Result, anyhow};
use rand::seq::IndexedRandom;
use std::time::Duration;
use tracing::{error, info};

use crate::{
    cli::{Cli, Command},
    download,
    fixtures::{control_client::ControlClient, lib, service::Service},
    report::{DestinationTable, RowStatus},
};
use gnosis_vpn_lib::connection::destination::Destination;

const SERVICE_TIMEOUT: Duration = Duration::from_secs(30);
const SAFE_TIMEOUT: Duration = Duration::from_secs(60);
const NODE_RUNNING_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const NODE_FUNDING_TIMEOUT: Duration = Duration::from_secs(30);
const READY_DESTINATIONS_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const FINAL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(180);
const DISCONNECTION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct SystemTestWorkflow {
    cli: Cli,
    client: ControlClient,
}

impl SystemTestWorkflow {
    pub async fn new(cli: Cli) -> Result<Self> {
        let (gnosis_bin, socket_path) = lib::prepare_configs().await?;
        let client = ControlClient::new(socket_path.clone());

        Service::spawn(&gnosis_bin, &cli.shared, &socket_path)?;

        Ok(Self { cli, client })
    }

    pub async fn run(self) -> Result<()> {
        self.ensure_daemon_ready().await?;

        let readiness = self
            .client
            .wait_for_ready_destinations(READY_DESTINATIONS_TIMEOUT)
            .await?;

        info!(
            ready = readiness.ready().len(),
            not_ready = readiness.not_ready().len(),
            "destinations ready to be used",
        );

        let mut readiness_report = DestinationTable::new(&[]);
        for destination in readiness.ready() {
            readiness_report.add_row(destination.get_meta("location"), RowStatus::Ready, Vec::new());
        }
        for destination in readiness.not_ready() {
            readiness_report.add_row(destination.get_meta("location"), RowStatus::NotReady, Vec::new());
        }
        info!("\n\n{}", readiness_report.render());

        let mut connection_report = DestinationTable::new(&[]);
        let successful_destinations = self
            .verify_all_destinations(readiness.ready(), &mut connection_report)
            .await?;

        info!("\n\n{}", connection_report.render());

        let destination = self.select_destination(&successful_destinations)?;
        self.establish_connection(&destination, FINAL_CONNECTION_TIMEOUT)
            .await?;

        self.get_public_ip().await?;

        match &self.cli.command {
            Command::Download(args) => download::run_downloads(&self.cli.shared, args).await?,
        };

        self.close_connection(DISCONNECTION_TIMEOUT).await?;

        Ok(())
    }

    async fn ensure_daemon_ready(&self) -> Result<()> {
        self.client.wait_for_service_running(SERVICE_TIMEOUT).await?;
        self.client.wait_for_safe_created(SAFE_TIMEOUT).await?;
        self.client.wait_for_node_running(NODE_RUNNING_TIMEOUT).await?;
        self.client.wait_for_node_funding(NODE_FUNDING_TIMEOUT).await?;
        Ok(())
    }

    async fn verify_all_destinations(
        &self,
        destinations: &[Destination],
        report: &mut DestinationTable,
    ) -> Result<Vec<Destination>> {
        let mut successful = Vec::new();
        for destination in destinations {
            match self.try_connect(destination, CONNECTION_TIMEOUT).await {
                Ok(_) => {
                    info!(dest = %destination, "connection established");
                    report.add_row(destination.get_meta("location"), RowStatus::Success, Vec::new());
                    successful.push(destination.clone());
                }
                Err(error) => {
                    error!(dest = %destination, ?error, "failed to establish connection");
                    report.add_row(
                        destination.get_meta("location"),
                        RowStatus::Failure(format!("{error:?}")),
                        Vec::new(),
                    );
                }
            }
        }

        Ok(successful)
    }

    async fn try_connect(&self, destination: &Destination, timeout: Duration) -> Result<()> {
        info!(
            dest = %destination,
            state = ?self.client.connect(destination.address).await?,
            "initiated connection",
        );

        self.client.wait_for_connection_established(destination, timeout).await
    }

    fn select_destination(&self, successful_destinations: &[Destination]) -> Result<Destination> {
        successful_destinations
            .choose(&mut rand::rng())
            .cloned()
            .ok_or_else(|| anyhow!("no destinations available to select from"))
    }

    async fn establish_connection(&self, destination: &Destination, timeout: Duration) -> Result<()> {
        info!(dest = %destination, "establishing primary connection");
        self.try_connect(destination, timeout).await
    }

    async fn close_connection(&self, timeout: Duration) -> Result<()> {
        info!("closing connection");
        self.client.wait_for_disconnection(timeout).await
    }

    async fn get_public_ip(&self) -> Result<String> {
        info!("querying public IP via the echo service");
        match lib::fetch_public_ip(&self.cli.shared.ip_echo_url, self.cli.shared.proxy.as_ref()).await {
            Ok(ip) => {
                info!(?ip, "found public IP");
                Ok(ip)
            }
            Err(error) => Err(error),
        }
    }
}
