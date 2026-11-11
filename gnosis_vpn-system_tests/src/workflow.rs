use anyhow::{Result, anyhow};
use std::time::Duration;
use tracing::info;

use crate::{
    cli::{Cli, Command},
    download,
    fixtures::{
        control_client::ControlClient,
        lib,
        service::{Service, ServiceGuard},
    },
    report::{ReportTable, RowStatus},
};
use gnosis_vpn_lib::connection::destination::Destination;

const SERVICE_TIMEOUT: Duration = Duration::from_secs(30);
const SAFE_TIMEOUT: Duration = Duration::from_mins(1);
const NODE_RUNNING_TIMEOUT: Duration = Duration::from_mins(30);
const NODE_FUNDING_TIMEOUT: Duration = Duration::from_secs(30);
const READY_DESTINATIONS_TIMEOUT: Duration = Duration::from_mins(10);
const QUICK_PROBE_TIMEOUT: Duration = Duration::from_mins(1);
const QUICK_PROBE_COUNT: usize = 2;
const CONNECTION_TIMEOUT: Duration = Duration::from_mins(10);
const DOWNLOAD_SIZE_BYTES: u64 = 250_000;
const DISCONNECTION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct SystemTestWorkflow {
    cli: Cli,
    client: ControlClient,
    _service: ServiceGuard,
}

impl SystemTestWorkflow {
    pub async fn new(cli: Cli) -> Result<Self> {
        let (gnosis_bin_root, socket_path) = lib::prepare_configs().await?;
        let ctl_binary = lib::find_binary("gnosis_vpn-ctl")?;
        let client = ControlClient::new(socket_path.clone(), ctl_binary, &cli.shared.worker_user)?;

        let service = Service::spawn(&gnosis_bin_root, &cli.shared, &socket_path)?;

        Ok(Self {
            cli,
            client,
            _service: service,
        })
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

        let mut readiness_report = ReportTable::new("destination", &[]);
        for destination in readiness.ready() {
            readiness_report.add_row(
                destination.get_meta("location").unwrap_or("<unknown>".to_string()),
                RowStatus::Ready,
                Vec::new(),
            );
        }
        for destination in readiness.not_ready() {
            readiness_report.add_row(
                destination.get_meta("location").unwrap_or("<unknown>".to_string()),
                RowStatus::NotReady,
                Vec::new(),
            );
        }
        info!("\n\n{}", readiness_report.render());

        let destination = self.select_best_probed(readiness.ready()).await?;
        self.establish_connection(&destination, CONNECTION_TIMEOUT).await?;

        info!(size_bytes = DOWNLOAD_SIZE_BYTES, "downloading through the tunnel");
        lib::download_file(DOWNLOAD_SIZE_BYTES).await?;
        info!(size_bytes = DOWNLOAD_SIZE_BYTES, "download finished");

        match &self.cli.command {
            Some(Command::Download(args)) => download::run_downloads(args).await?,
            None => info!("no additional commands to run"),
        };

        self.close_connection(DISCONNECTION_TIMEOUT).await?;
        self.client.stop().await?;

        Ok(())
    }

    async fn ensure_daemon_ready(&self) -> Result<()> {
        self.client.wait_for_service_running(SERVICE_TIMEOUT).await?;

        self.client.start().await?;

        self.client.wait_for_safe_created(SAFE_TIMEOUT).await?;
        self.client.wait_for_node_running(NODE_RUNNING_TIMEOUT).await?;
        self.client.wait_for_node_funding(NODE_FUNDING_TIMEOUT).await?;

        Ok(())
    }

    /// Quick-probes random ready destinations and picks the one with the fastest status round trip.
    async fn select_best_probed(&self, ready: &[Destination]) -> Result<Destination> {
        let candidates = ready.get(..QUICK_PROBE_COUNT).ok_or_else(|| {
            anyhow!(
                "need {QUICK_PROBE_COUNT} ready destinations to probe, have {}",
                ready.len()
            )
        })?;

        let mut report = ReportTable::new("destination", &["status rtt"]);
        let mut best: Option<(&Destination, Duration)> = None;
        for destination in candidates {
            let check = self.client.quick_probe(destination, QUICK_PROBE_TIMEOUT).await?;
            info!(dest = %destination, rtt = ?check.status_rtt, "quick probe succeeded");
            report.add_row(
                destination.get_meta("location").unwrap_or("<unknown>".to_string()),
                RowStatus::Success,
                vec![format!("{:?}", check.status_rtt)],
            );

            let is_fastest = best.is_none_or(|(_, rtt)| check.status_rtt < rtt);
            if is_fastest {
                best = Some((destination, check.status_rtt));
            }
        }
        info!("\n\n{}", report.render());

        best.map(|(destination, _)| destination.clone())
            .ok_or_else(|| anyhow!("no destination was probed"))
    }

    async fn try_connect(&self, destination: &Destination, timeout: Duration) -> Result<()> {
        info!(
            dest = %destination,
            state = ?self.client.connect(destination.id.clone()).await?,
            "initiated connection",
        );

        self.client.wait_for_connection_established(destination, timeout).await
    }

    async fn establish_connection(&self, destination: &Destination, timeout: Duration) -> Result<()> {
        info!(dest = %destination, "establishing primary connection");
        self.try_connect(destination, timeout).await
    }

    async fn close_connection(&self, timeout: Duration) -> Result<()> {
        info!("closing connection");
        self.client.wait_for_disconnection(timeout).await
    }
}
