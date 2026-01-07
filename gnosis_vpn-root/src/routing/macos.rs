use std::sync::Arc;

use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
// use gnosis_vpn_lib::dirs;
use gnosis_vpn_lib::{event, worker};

use std::net::Ipv4Addr;

use crate::wg_tooling;

use super::{Error, Routing};

pub fn build_firewall_router(worker: worker::Worker, wg_data: event::WireGuardData) -> Result<impl Routing, Error> {
    let pf = pfctl::PfCtl::new()?;
    Ok(Firewall {
        fw: Arc::new(std::sync::Mutex::new(pf)),
        worker,
        wg_data,
    })
}

pub fn static_fallback_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> impl Routing {
    FallbackRouter { wg_data, peer_ips }
}

// const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

pub struct Firewall {
    fw: Arc<std::sync::Mutex<pfctl::PfCtl>>,
    #[allow(dead_code)]
    worker: worker::Worker,
    wg_data: event::WireGuardData,
}

pub struct FallbackRouter {
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

#[async_trait::async_trait]
impl Routing for Firewall {
    /**
     * Refactor logic to use:
     * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
     */
    #[tracing::instrument(name = "Firewall::setup",level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn setup(&self) -> Result<(), Error> {
        // 1. generate wg quick content
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            // true to route all traffic
            false, // START WITH TRUE TO KILL YOURSELF
            None,
        );

        // 2. run wg-quick up
        wg_tooling::up(wg_quick_content).await?;

        // 3. determine interface
        let (device, gateway) = interface().await?;

        tracing::info!(%device, ?gateway, "Determined default interface");

        // Create a PfCtl instance to control PF with:
        let mut pf = self
            .fw
            .lock()
            .ok()
            .ok_or(Error::General("Failed to acquire lock on pfctl".into()))?;

        // Enable the firewall, equivalent to the command "pfctl -e":
        pf.try_enable()?;

        // // Add an anchor rule for packet filtering rules into PF. This will fail if it already exists,
        // // use `try_add_anchor` to avoid that:
        // let anchor_name = "testing-out-pfctl";
        // pf.add_anchor(anchor_name, pfctl::AnchorKind::Filter).unwrap();

        // // Create a packet filtering rule matching all packets on the "lo0" interface and allowing
        // // them to pass:
        // let rule = pfctl::FilterRuleBuilder::default()
        //     .action(pfctl::FilterRuleAction::Pass)
        //     .interface("lo0")
        //     .build()
        //     .unwrap();

        // // Add the filterig rule to the anchor we just created.
        // pf.add_rule(anchor_name, &rule).unwrap();

        Ok(())
    }

    #[tracing::instrument(name = "Firewall::teardown",level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn teardown(&self) -> Result<(), Error> {
        // 1. run wg-quick down
        wg_tooling::down().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
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
            "route -n add --host {relayer_ip} {gateway}",
            relayer_ip = relayer_ip,
            gateway = gw,
        ),
        None => format!(
            "route -n add -host {relayer_ip} -interface {device}",
            relayer_ip = relayer_ip,
            device = device
        ),
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (_device, _gateway): (String, Option<String>)) -> String {
    format!("route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
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
