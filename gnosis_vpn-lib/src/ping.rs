use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use crate::shell_command_ext::ShellCommandExt;

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
    #[error("Failed to parse duration from ping output")]
    DurationParserFailed,
    #[error("Failed to parse duration: {0}")]
    DurationFromString(#[from] std::num::ParseFloatError),
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
    let available = Command::new("which").arg("ping").run().await;

    match available {
        Ok(_) => ping_using_cmd(opts).await,
        Err(error) => {
            tracing::warn!(?error, "Unable to use system ping cmd - fallback to internal ping");
            ping_using_ping_crate(opts)
        }
    }
}

async fn ping_using_cmd(opts: &Options) -> Result<Duration, Error> {
    let mut cmd = Command::new("ping");
    cmd.arg("-c").arg(opts.seq_count.to_string());
    let timeout_str = opts.timeout.as_secs().to_string();
    #[cfg(target_os = "linux")]
    {
        cmd.arg("-W").arg(timeout_str);
    }
    #[cfg(target_os = "macos")]
    {
        cmd.arg("-t").arg(timeout_str);
    }
    let output = cmd
        .arg(opts.address.to_string())
        .run_stdout()
        .await
        .map_err(|_| Error::Timeout)?;
    parse_duration(output)
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

pub fn parse_duration(duration: String) -> Result<Duration, Error> {
    for line in duration.lines() {
        if line.contains("rtt") || line.contains("round-trip") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() < 2 {
                continue;
            }
            let numbers_part = parts[1].trim();
            let first_number_str = numbers_part
                .split('/')
                .nth(1)
                .ok_or(Error::DurationParserFailed)?
                .trim();
            let first_number = first_number_str.parse::<f64>()?;
            let microseconds = (first_number * 1000.0) as u64;
            return Ok(Duration::from_micros(microseconds));
        }
    }
    Err(Error::DurationParserFailed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_duration_on_single_ping() -> anyhow::Result<()> {
        let duration_linux = r#####"
PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.
64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=13.1 ms

--- 1.1.1.1 ping statistics ---
1 packets transmitted, 1 received, 0% packet loss, time 0ms
rtt min/avg/max/mdev = 13.135/13.135/13.135/0.000 ms
"#####;
        let duration_mac = r#####"
PING 1.1.1.1 (1.1.1.1): 56 data bytes
64 bytes from 1.1.1.1: icmp_seq=0 ttl=57 time=19.540 ms

--- 1.1.1.1 ping statistics ---
1 packets transmitted, 1 packets received, 0.0% packet loss
round-trip min/avg/max/stddev = 19.540/19.540/19.540/nan ms
"#####;

        let d_linux = super::parse_duration(duration_linux.to_string())?;
        let d_mac = super::parse_duration(duration_mac.to_string())?;

        assert_eq!(d_linux, std::time::Duration::from_micros(13135));
        assert_eq!(d_mac, std::time::Duration::from_micros(19540));

        Ok(())
    }

    #[test]
    fn parse_duration_on_multiple_pings() -> anyhow::Result<()> {
        let duration_linux = r#####"
PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.
64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=13.2 ms
64 bytes from 1.1.1.1: icmp_seq=2 ttl=57 time=10.1 ms
64 bytes from 1.1.1.1: icmp_seq=3 ttl=57 time=9.46 ms

--- 1.1.1.1 ping statistics ---
3 packets transmitted, 3 received, 0% packet loss, time 2003ms
rtt min/avg/max/mdev = 9.458/10.937/13.208/1.629 ms
"#####;
        let duration_mac = r#####"
        Last login: Tue Jan 27 14:00:28 2026 from 192.168.200.212
este@officemac ~ % ping -c3 1.1.1.1
PING 1.1.1.1 (1.1.1.1): 56 data bytes
64 bytes from 1.1.1.1: icmp_seq=0 ttl=57 time=57.212 ms
64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=17.999 ms
64 bytes from 1.1.1.1: icmp_seq=2 ttl=57 time=25.403 ms

--- 1.1.1.1 ping statistics ---
3 packets transmitted, 3 packets received, 0.0% packet loss
round-trip min/avg/max/stddev = 17.999/33.538/57.212/17.011 ms
"#####;

        let d_linux = super::parse_duration(duration_linux.to_string())?;
        let d_mac = super::parse_duration(duration_mac.to_string())?;

        assert_eq!(d_linux, std::time::Duration::from_micros(10937));
        assert_eq!(d_mac, std::time::Duration::from_micros(33538));

        Ok(())
    }
}
