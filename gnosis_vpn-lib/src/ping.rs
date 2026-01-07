use serde::{Deserialize, Serialize};
use tokio::process::Command;
use thiserror::Error;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use crate::shell_command_ext::{ShellCommandExt};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub address: IpAddr,
    pub timeout: Duration,
    pub ttl: u32,
    pub seq_count: u16,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Ping failed")]
    PingFailed(#[from] ping::Error),
    #[error("Ping timed out")]
    Timeout,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            address: IpAddr::V4(Ipv4Addr::new(10, 128, 0, 1)),
            timeout: Duration::from_secs(15),
            ttl: 6,
            seq_count: 1,
        }
    }
}

#[tracing::instrument(name = "ping", ret)]
pub async fn ping(opts: &Options) -> Result<Duration, Error> {
    // prefer system ping as it seems way more robust that ping crate
    let available = Command::new("which")
        .arg("ping")
        .run().await;

    match available {
        Ok(_) => {
            ping_using_cmd(opts)
        }
        Err(error) => {
            tracing::warn!(?error, "Unable to use system ping cmd - fallback to internal ping");
            ping_using_ping_crate(opts)
        }
        }

}

async fn ping_using_cmd(opts: &Options) -> Result<Duration, Error> {
            let mut cmd = Command::new("ping").arg("-c").arg("1")
    #[cfg(target_os = "linux")]
    {
        cmd = cmd.arg("-W").arg(opts.timeout)
    }
    #[cfg(target_os = "macos")]
    {
        cmd = cmd.arg("-t").arg(opts.timeout)
    }
    cmd.run().await.map_err(|_| Error::Timeout)
}

fn ping_using_ping_crate(opts: &Options) -> Result<Duration, Error> {
    let mut builder = ping::new(opts.address);
    let mut ping = builder.timeout(opts.timeout).ttl(opts.ttl).seq_cnt(opts.seq_count);
    #[cfg(target_os = "linux")]
    {
        ping = ping.socket_type(ping::RAW);
    }
    #[cfg(target_os = "macos")]
    {
        ping = ping.socket_type(ping::DGRAM);
    }
    ping.send().map(|p| p.rtt).map_err(Error::from)
}
