use anyhow::{Result, anyhow};
use std::time::{Duration, Instant};
use tracing::{error, info};

use crate::{
    cli::DownloadArgs,
    fixtures::lib,
    report::{ReportTable, RowStatus},
};

pub async fn run_downloads(cli: &crate::cli::SharedArgs, args: &DownloadArgs) -> Result<()> {
    let mut report = ReportTable::new("file size", &["avg (s)", "min (s)", "max (s)", "successes", "failures"]);
    let mut had_total_failure = false;

    for idx in 0..args.attempts {
        let file_size = args.min_size_bytes * (args.size_factor.pow(idx) as u64);
        let mut stats = DownloadStats::new();

        for attempt in 0..args.repetitions {
            info!(
                %file_size,
                attempt = attempt + 1,
                    total = args.repetitions,
                "starting download sample"
            );

            let start = Instant::now();
            match lib::download_file(file_size, cli.proxy.as_ref()).await {
                Ok(_) => {
                    let elapsed = start.elapsed();
                    info!(%file_size, elapsed = ?elapsed, "sample download succeeded");
                    stats.record_success(elapsed);
                }
                Err(error) => {
                    let elapsed = start.elapsed();
                    error!(%file_size, elapsed = ?elapsed, ?error, "sample download failed");
                    stats.record_failure();
                }
            }
        }

        if stats.successes.is_empty() {
            had_total_failure = true;
        }

        report.add_row(format!("{} bytes", file_size), stats.status(), stats.to_values());
    }

    info!("\n\nDownload performance:\n{}", report.render());

    if had_total_failure {
        Err(anyhow!("one or more download batches failed"))
    } else {
        Ok(())
    }
}

struct DownloadStats {
    successes: Vec<Duration>,
    failures: usize,
}

impl DownloadStats {
    fn new() -> Self {
        Self {
            successes: Vec::new(),
            failures: 0,
        }
    }

    fn record_success(&mut self, elapsed: Duration) {
        self.successes.push(elapsed);
    }

    fn record_failure(&mut self) {
        self.failures += 1;
    }

    fn status(&self) -> RowStatus {
        if self.failures == 0 && !self.successes.is_empty() {
            RowStatus::Success
        } else if self.successes.is_empty() {
            RowStatus::Failure("all attempts failed".into())
        } else {
            RowStatus::Failure(format!("{} failure(s)", self.failures))
        }
    }

    fn to_values(&self) -> Vec<String> {
        vec![
            Self::format_duration(self.average_duration()),
            Self::format_duration(self.min_duration()),
            Self::format_duration(self.max_duration()),
            self.successes.len().to_string(),
            self.failures.to_string(),
        ]
    }

    fn average_duration(&self) -> Option<Duration> {
        if self.successes.is_empty() {
            None
        } else {
            let total_secs: f64 = self.successes.iter().map(|d| d.as_secs_f64()).sum();
            Some(Duration::from_secs_f64(total_secs / self.successes.len() as f64))
        }
    }

    fn min_duration(&self) -> Option<Duration> {
        self.successes.iter().copied().min()
    }

    fn max_duration(&self) -> Option<Duration> {
        self.successes.iter().copied().max()
    }

    fn format_duration(duration: Option<Duration>) -> String {
        duration
            .map(|d| format!("{:.3}", d.as_secs_f64()))
            .unwrap_or_else(|| "-".to_string())
    }
}
