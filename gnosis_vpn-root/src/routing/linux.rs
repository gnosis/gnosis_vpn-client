use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{event, hopr::hopr_lib::async_trait, worker};

use std::net::Ipv4Addr;

use crate::wg_tooling;

use super::{Error, Routing};

// const MARK: &str = "0xDEAD";

pub fn build_userspace_router(_worker: worker::Worker, _wg_data: event::WireGuardData) -> Result<Router, Error> {
    Err(Error::NotImplemented)
}

pub fn static_fallback_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> impl Routing {
    FallbackRouter { wg_data, peer_ips }
}

// TOOD remove allow dead code once implemented
#[allow(dead_code)]
pub struct Router {
    worker: worker::Worker,
    wg_data: event::WireGuardData,
}

pub struct FallbackRouter {
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
#[async_trait]
impl Routing for Router {
    async fn setup(&self) -> Result<(), Error> {
        // 1. generate wg quick content
        //        let wg_quick_content = self.wg_data.wg.to_file_string(
        //            &self.wg_data.interface_info,
        //            &self.wg_data.peer_info,
        //            // true to route all traffic
        //            false,
        //        );
        // 2. run wg-quick up
        // wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), Error> {
        // 1. run wg-quick down
        //  wg_tooling::down().await?;
        Ok(())
    }
}

#[async_trait]
impl Routing for FallbackRouter {
    async fn setup(&self) -> Result<(), Error> {
        let interface_gateway = interface().await?;
        let mut extra = self
            .peer_ips
            .iter()
            .map(|ip| pre_up_routing(ip, interface_gateway.clone()))
            .collect::<Vec<String>>();
        extra.extend(
            self.peer_ips
                .iter()
                .map(|ip| post_down_routing(ip, interface_gateway.clone()))
                .collect::<Vec<String>>(),
        );

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, Some(extra));
        wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), Error> {
        wg_tooling::down().await?;
        Ok(())
    }
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    match gateway {
        Some(gw) => format!(
            "PreUp = ip route add {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device
        ),
        None => format!(
            "PreUp = ip route add {relayer_ip} dev {device}",
            relayer_ip = relayer_ip,
            device = device
        ),
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    match gateway {
        Some(gw) => format!(
            "PostDown = ip route del {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device,
        ),
        None => format!(
            "PostDown = ip route del {relayer_ip} dev {device}",
            relayer_ip = relayer_ip,
            device = device,
        ),
    }
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("ip")
        .arg("route")
        .arg("show")
        .arg("default")
        .run_stdout()
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();

    let device_index = parts.iter().position(|&x| x == "dev");
    let via_index = parts.iter().position(|&x| x == "via");

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
        let output = "default via 192.168.101.1 dev wlp2s0 proto dhcp src 192.168.101.202 metric 600 ";

        let (device, gateway) = super::parse_interface(output)?;

        assert_eq!(device, "wlp2s0");
        assert_eq!(gateway, Some("192.168.101.1".to_string()));
        Ok(())
    }
}
