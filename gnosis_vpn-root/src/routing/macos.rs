//! macOS routing implementation using `wg-quick` hook commands.
//!
//! Builds a static router that installs per-peer host routes and
//! default-route overrides via `route` commands for split-tunnel behavior.
use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{event, worker};

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::{Error, Routing};
use crate::wg_tooling;

/// Dynamic routing not available on macOS.
pub fn dynamic_router(
    _state_home: PathBuf,
    _worker: worker::Worker,
    _wg_data: event::WireGuardData,
) -> Result<DynamicRouter, Error> {
    Err(Error::NotAvailable)
}

pub struct DynamicRouter {}

/// Builds a static macOS router without a worker handle.
pub fn static_router(state_home: PathBuf, wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> StaticRouter {
    StaticRouter {
        state_home,
        wg_data,
        peer_ips,
    }
}

/// macOS routing implementation that programs host routes via `wg-quick` hooks.
pub struct StaticRouter {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

#[async_trait]
impl Routing for StaticRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        let interface_gateway = interface().await?;
        let extra = build_static_extra_lines(&self.peer_ips, interface_gateway);

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);
        wg_tooling::up(self.state_home.clone(), wg_quick_content).await?;

        Ok(())
    }

    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(self.state_home.clone(), logs).await?;
        Ok(())
    }
}

/// Dynamic routing not available on macOS.
#[async_trait]
impl Routing for DynamicRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        Err(Error::NotAvailable)
    }

    async fn teardown(&mut self, _logs: Logs) -> Result<(), Error> {
        Err(Error::NotAvailable)
    }
}

fn build_static_extra_lines(peer_ips: &[Ipv4Addr], interface_gateway: (String, Option<String>)) -> Vec<String> {
    // take over routing from wg-quick
    let mut extra = vec!["Table = off".to_string()];
    // default routes are added PostUp on wg interface
    extra.push("PostUp = route -n add -inet 0.0.0.0/1 -interface %i".to_string());
    extra.push("PostUp = route -n add -inet 128.0.0.0/1 -interface %i".to_string());
    // add routes exceptions to all connected peers
    extra.extend(peer_ips.iter().map(|ip| pre_up_routing(ip, interface_gateway.clone())));
    // remove routes exceptions on PostDown
    extra.extend(
        peer_ips
            .iter()
            .map(|ip| post_down_routing(ip, interface_gateway.clone())),
    );
    extra
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    match gateway {
        Some(gw) => format!(
            // NOTE: difference to linux: route command acts idempotent in a way that it always returns exit code 0 - even if a route already exists
            "PreUp = route -n add -host {relayer_ip} {gateway}",
            relayer_ip = relayer_ip,
            gateway = gw,
        ),
        None => format!(
            // NOTE: difference to linux: route command acts idempotent in a way that it always returns exit code 0 - even if a route already exists
            "PreUp = route -n add -host {relayer_ip} -interface {device}",
            relayer_ip = relayer_ip,
            device = device
        ),
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (_device, _gateway): (String, Option<String>)) -> String {
    format!("PostDown = route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("route")
        .arg("-n")
        .arg("get")
        .arg("0.0.0.0")
        .run_stdout(Logs::Print)
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();
    let device_index = parts.iter().position(|&x| x == "interface:");
    let via_index = parts.iter().position(|&x| x == "gateway:");
    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
    Ok((device, gateway))
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = r#"
           route to: default
        destination: default
               mask: default
            gateway: 192.168.178.1
          interface: en1
              flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
         recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
               0         0         0         0         0         0      1500         0
        "#;

        let (device, gateway) = super::parse_interface(output)?;

        assert_eq!(device, "en1");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }
}
