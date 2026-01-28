//! macOS routing implementation using `wg-quick` hook commands.
//!
//! Builds a static router that installs per-peer host routes and
//! default-route overrides via `route` commands for split-tunnel behavior.
use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{event, worker};

use crate::wg_tooling;
use std::net::Ipv4Addr;

use super::{Error, Routing};

/// Dynamic routing not available on macOS.
pub fn dynamic_router(_worker: worker::Worker, _wg_data: event::WireGuardData ) -> Result<DynamicRouter, Error> {
    Err(Error::NotAvailable)
}

pub struct DynamicRouter {}

/// Builds a static macOS router without a worker handle.
pub fn static_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> StaticRouter {
    StaticRouter { wg_data, peer_ips }
}

/// macOS routing implementation that programs host routes via `wg-quick` hooks.
pub struct StaticRouter {
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
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, Some(extra));
        wg_tooling::up(wg_quick_content).await?;

        Ok(())
    }

    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        wg_tooling::down(logs).await?;
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

fn build_static_extra_lines(
    peer_ips: &[Ipv4Addr],
    interface_gateway: (String, Option<String>),
) -> Vec<String> {
    // take over routing from wg-quick
    let mut extra = vec!["Table = off".to_string()];
    // default routes are added PostUp on wg interface
    extra.extend(default_route_hook_lines("PostUp", "add"));
    // add routes exceptions to all connected peers
    extra.extend(peer_ips.iter().map(|ip| pre_up_routing(ip, interface_gateway.clone())));
    // remove routes exceptions on PostDown
    extra.extend(peer_ips.iter().map(|ip| post_down_routing(ip, interface_gateway.clone())));
    extra
}

fn default_route_hook_lines(hook: &str, action: &str) -> Vec<String> {
    ["0.0.0.0/1", "128.0.0.0/1"]
        .iter()
        .map(|cidr| format!("{hook} = route -n {action} -inet {cidr} -interface %i"))
        .collect()
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    match gateway {
        Some(gw) => format!(
            "PreUp = route -n add -host {relayer_ip} {gateway}",
            relayer_ip = relayer_ip,
            gateway = gw,
        ),
        None => format!(
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
    use std::net::Ipv4Addr;
    use std::path::PathBuf;

    use gnosis_vpn_lib::{event, wireguard, worker};

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

    #[test]
    fn builds_peer_route_lines_include_pre_down() {
        let interface_gateway = ("en0".to_string(), Some("192.168.88.1".to_string()));

        let pre_up = super::pre_up_routing(&Ipv4Addr::new(10, 0, 0, 1), interface_gateway.clone());
        let pre_down = super::pre_down_routing(&Ipv4Addr::new(10, 0, 0, 1), interface_gateway.clone());
        let post_down = super::post_down_routing(&Ipv4Addr::new(10, 0, 0, 1), interface_gateway);

        assert_eq!(pre_up, "PreUp = route -n add -host 10.0.0.1 192.168.88.1");
        assert_eq!(pre_down, "PreDown = route -n delete -host 10.0.0.1");
        assert_eq!(post_down, "PostDown = route -n delete -host 10.0.0.1");
    }

    #[test]
    fn build_static_extra_lines_include_table_off() {
        let peer_ips = [Ipv4Addr::new(10, 0, 0, 1)];
        let interface_gateway = ("en0".to_string(), Some("192.168.88.1".to_string()));

        let extra = super::build_static_extra_lines(&peer_ips, interface_gateway, wireguard::WG_INTERFACE);

        assert_eq!(extra[0], "Table = off");
        assert_eq!(extra.len(), 10);
        assert!(
            extra
                .iter()
                .any(|line| { line == "PreUp = route -n add -inet 0.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(
            extra
                .iter()
                .any(|line| { line == "PreUp = route -n add -inet 128.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(
            extra
                .iter()
                .any(|line| { line == "PreDown = route -n delete -inet 0.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(
            extra
                .iter()
                .any(|line| { line == "PreDown = route -n delete -inet 128.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(
            extra
                .iter()
                .any(|line| { line == "PostDown = route -n delete -inet 0.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(
            extra
                .iter()
                .any(|line| { line == "PostDown = route -n delete -inet 128.0.0.0/1 -interface wg0_gnosisvpn" })
        );
        assert!(extra.iter().any(|line| line.contains("PreUp = route -n add -host")));
        assert!(
            extra
                .iter()
                .any(|line| line.contains("PreDown = route -n delete -host"))
        );
        assert!(extra.iter().any(|line| line.contains("PostDown")));
    }

    #[test]
    fn build_firewall_router_returns_static_router() {
        let worker = worker::Worker {
            uid: 1000,
            gid: 1000,
            group_name: "gnosisvpn".to_string(),
            binary: "/usr/local/bin/gnosis_vpn-worker".to_string(),
            home: PathBuf::from("/tmp"),
        };
        let wg_data = event::WireGuardData {
            wg: wireguard::WireGuard::new(
                wireguard::Config {
                    listen_port: None,
                    force_private_key: None,
                    allowed_ips: None,
                },
                wireguard::KeyPair {
                    priv_key: "priv_key".to_string(),
                    public_key: "public_key".to_string(),
                },
            ),
            interface_info: wireguard::InterfaceInfo {
                address: "10.0.0.1/32".to_string(),
            },
            peer_info: wireguard::PeerInfo {
                public_key: "peer_key".to_string(),
                endpoint: "127.0.0.1:51820".to_string(),
            },
        };

        let router: super::StaticRouter =
            super::build_firewall_router(worker, wg_data, vec![Ipv4Addr::new(10, 0, 0, 1)]).expect("router");
        let _ = router;
    }
}
