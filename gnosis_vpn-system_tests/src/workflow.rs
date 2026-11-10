use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::{path::Path, time::Duration};
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
use gnosis_vpn_ipc::{Address, Destination, PixPhase};

const SERVICE_TIMEOUT: Duration = Duration::from_secs(30);
const SAFE_TIMEOUT: Duration = Duration::from_mins(1);
const NODE_RUNNING_TIMEOUT: Duration = Duration::from_mins(30);
const NODE_FUNDING_TIMEOUT: Duration = Duration::from_secs(30);
const DISCOVERY_TIMEOUT: Duration = Duration::from_mins(5);
const GEOLOCATION_TIMEOUT: Duration = Duration::from_mins(2);
const READY_DESTINATIONS_TIMEOUT: Duration = Duration::from_mins(10);
const QUICK_PROBE_TIMEOUT: Duration = Duration::from_mins(1);
const QUICK_PROBE_COUNT: usize = 2;
const CONNECTION_TIMEOUT: Duration = Duration::from_mins(10);
const DOWNLOAD_SIZE_BYTES: u64 = 250_000;
const DISCONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
const PIX_FUNDED_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Debug, Deserialize)]
struct ExpectedExits {
    exit: Vec<ExpectedExit>,
}

#[derive(Debug, Deserialize)]
struct ExpectedExit {
    address: String,
    country: String,
}

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

        match &self.cli.shared.expected_exits {
            Some(path) => self.verify_exit_discovery(path).await?,
            None => info!("no expected exits for this network, skipping discovery checks"),
        }

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
            readiness_report.add_row(destination.id.to_string(), RowStatus::Ready, Vec::new());
        }
        for destination in readiness.not_ready() {
            readiness_report.add_row(destination.id.to_string(), RowStatus::NotReady, Vec::new());
        }
        info!("\n\n{}", readiness_report.render());

        let destination = self.select_best_probed(readiness.ready()).await?;
        self.establish_connection(&destination, CONNECTION_TIMEOUT).await?;

        info!(size_bytes = DOWNLOAD_SIZE_BYTES, "downloading through the tunnel");
        lib::download_file(DOWNLOAD_SIZE_BYTES, self.cli.shared.proxy.as_ref()).await?;
        info!(size_bytes = DOWNLOAD_SIZE_BYTES, "download finished");

        match &self.cli.command {
            Some(Command::Download(args)) => download::run_downloads(&self.cli.shared, args).await?,
            None => info!("no additional commands to run"),
        };

        if self.cli.shared.expect_pix {
            self.verify_pix_funded(&destination).await?;
        }

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

    /// Every expected exit must be discovered from the registry and placed in its expected country.
    async fn verify_exit_discovery(&self, path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        let expected = toml::from_str::<ExpectedExits>(&content)?.exit;
        let addresses = expected
            .iter()
            .map(|exit| exit.address.parse::<Address>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow!("invalid expected exit address in {}: {error}", path.display()))?;

        let discovered = self
            .client
            .wait_for_discovered_exits(&addresses, DISCOVERY_TIMEOUT)
            .await?;
        let mut discovery_report = ReportTable::new("exit", &[]);
        for (exit, destination) in expected.iter().zip(&discovered) {
            let status = match destination {
                Some(_) => RowStatus::Success,
                None => RowStatus::Failure("not discovered".to_string()),
            };
            discovery_report.add_row(exit.address.clone(), status, Vec::new());
        }
        info!("\n\n{}", discovery_report.render());
        let undiscovered = discovered.iter().filter(|dest| dest.is_none()).count();
        if undiscovered > 0 {
            return Err(anyhow!(
                "{undiscovered} of {} expected exits not discovered",
                expected.len()
            ));
        }

        let countries = self
            .client
            .wait_for_exit_countries(&addresses, GEOLOCATION_TIMEOUT)
            .await?;
        let mut location_report = ReportTable::new("exit", &["expected", "resolved"]);
        let mut mislocated = 0;
        for (exit, country) in expected.iter().zip(&countries) {
            let resolved = country.clone().unwrap_or_else(|| "-".to_string());
            let status = if resolved == exit.country {
                RowStatus::Success
            } else {
                mislocated += 1;
                RowStatus::Failure("country mismatch".to_string())
            };
            location_report.add_row(exit.address.clone(), status, vec![exit.country.clone(), resolved]);
        }
        info!("\n\n{}", location_report.render());
        if mislocated > 0 {
            return Err(anyhow!("{mislocated} of {} expected exits mislocated", expected.len()));
        }

        Ok(())
    }

    async fn verify_pix_funded(&self, destination: &Destination) -> Result<()> {
        let stats = self.client.wait_for_pix_funded(destination, PIX_FUNDED_TIMEOUT).await?;

        let mut report = ReportTable::new(
            "destination",
            &["phase", "committed", "paid", "failed", "bytes bought", "cost"],
        );
        let is_funded = stats.as_ref().is_some_and(|s| s.phase == PixPhase::Funded);
        let row_status = if is_funded {
            RowStatus::Success
        } else {
            RowStatus::Failure("not funded".to_string())
        };
        let values = match &stats {
            Some(s) => vec![
                format!("{:?}", s.phase),
                s.chunks_committed.to_string(),
                s.chunks_paid.to_string(),
                s.chunks_failed.to_string(),
                s.bytes_bought.to_string(),
                s.cost.to_string(),
            ],
            None => vec!["no session".to_string()],
        };
        report.add_row(destination.id.to_string(), row_status, values);
        info!("\n\n{}", report.render());

        if !is_funded {
            return Err(anyhow!("pix chunks not paid within {PIX_FUNDED_TIMEOUT:?}"));
        }
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
                destination.id.to_string(),
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
