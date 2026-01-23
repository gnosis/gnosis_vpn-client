use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{event, worker};

use crate::wg_tooling;
use std::net::Ipv4Addr;

use super::{Error, Routing};

pub fn build_firewall_router(
    worker: worker::Worker,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<StaticRouter, Error> {
    let _ = worker;
    Ok(StaticRouter { wg_data, peer_ips })
}

pub fn static_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> StaticRouter {
    StaticRouter { wg_data, peer_ips }
}

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

        let utun_device = wg_interface_from_public_key(&self.wg_data.wg.key_pair.public_key).await?;
        apply_default_routes(&utun_device).await?;

        Ok(())
    }

    async fn teardown(&mut self) -> Result<(), Error> {
        match wg_interface_from_public_key(&self.wg_data.wg.key_pair.public_key).await {
            Ok(utun_device) => {
                if let Err(err) = remove_default_routes(&utun_device).await {
                    tracing::warn!(error = ?err, "failed to remove default routes");
                }
            }
            Err(err) => tracing::warn!(error = ?err, "failed to lookup utun device"),
        }

        wg_tooling::down().await?;
        Ok(())
    }
}

async fn wg_interface_from_public_key(public_key: &str) -> Result<String, Error> {
    let output = Command::new("wg")
        .arg("show")
        .arg("all")
        .arg("dump")
        .run_stdout()
        .await?;
    parse_wg_interface_from_dump(&output, public_key)
        .ok_or_else(|| Error::General(format!("Unable to find wg interface for public key {public_key}")))
}

fn parse_wg_interface_from_dump(output: &str, public_key: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let iface = parts.next()?;
        let _private_key = parts.next()?;
        let iface_public_key = parts.next()?;
        if iface_public_key == public_key {
            Some(iface.to_string())
        } else {
            None
        }
    })
}

fn build_static_extra_lines(peer_ips: &[Ipv4Addr], interface_gateway: (String, Option<String>)) -> Vec<String> {
    let mut extra = vec!["Table = off".to_string()];
    extra.extend(peer_ips.iter().map(|ip| pre_up_routing(ip, interface_gateway.clone())));
    extra.extend(
        peer_ips
            .iter()
            .map(|ip| pre_down_routing(ip, interface_gateway.clone())),
    );
    extra.extend(
        peer_ips
            .iter()
            .map(|ip| post_down_routing(ip, interface_gateway.clone())),
    );
    extra
}

fn default_route_specs(utun_device: &str, action: &str) -> Vec<Vec<String>> {
    ["0.0.0.0/1", "128.0.0.0/1"]
        .iter()
        .map(|cidr| {
            vec![
                "-q".to_string(),
                "-n".to_string(),
                action.to_string(),
                "-inet".to_string(),
                cidr.to_string(),
                "-interface".to_string(),
                utun_device.to_string(),
            ]
        })
        .collect()
}

async fn apply_default_routes(utun_device: &str) -> Result<(), Error> {
    for spec in default_route_specs(utun_device, "add") {
        Command::new("route").args(spec).run_stdout().await?;
    }
    Ok(())
}

async fn remove_default_routes(utun_device: &str) -> Result<(), Error> {
    for spec in default_route_specs(utun_device, "delete") {
        Command::new("route").args(spec).run_stdout().await?;
    }
    Ok(())
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

fn pre_down_routing(relayer_ip: &Ipv4Addr, (_device, _gateway): (String, Option<String>)) -> String {
    format!("PreDown = route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (_device, _gateway): (String, Option<String>)) -> String {
    format!("PostDown = route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("route")
        .arg("-n")
        .arg("get")
        .arg("0.0.0.0")
        .run_stdout()
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
    fn parses_wg_interface_from_dump() {
        let output = "utun9 private_key_a public_key_a 51820 0\nutun10 private_key_b public_key_b 51820 0";

        let iface = super::parse_wg_interface_from_dump(output, "public_key_b");

        assert_eq!(iface, Some("utun10".to_string()));
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

        let extra = super::build_static_extra_lines(&peer_ips, interface_gateway);

        assert_eq!(extra[0], "Table = off");
        assert_eq!(extra.len(), 4);
        assert!(extra.iter().any(|line| line.contains("PreUp")));
        assert!(extra.iter().any(|line| line.contains("PreDown")));
        assert!(extra.iter().any(|line| line.contains("PostDown")));
    }

    #[test]
    fn build_default_route_specs_for_add() {
        let specs = super::default_route_specs("utun8", "add");

        assert_eq!(
            specs,
            vec![
                vec![
                    "-q".to_string(),
                    "-n".to_string(),
                    "add".to_string(),
                    "-inet".to_string(),
                    "0.0.0.0/1".to_string(),
                    "-interface".to_string(),
                    "utun8".to_string(),
                ],
                vec![
                    "-q".to_string(),
                    "-n".to_string(),
                    "add".to_string(),
                    "-inet".to_string(),
                    "128.0.0.0/1".to_string(),
                    "-interface".to_string(),
                    "utun8".to_string(),
                ],
            ]
        );
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
