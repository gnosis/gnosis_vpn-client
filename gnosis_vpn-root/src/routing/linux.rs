use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{event, hopr::hopr_lib::async_trait, worker};

use std::net::Ipv4Addr;
use futures::TryStreamExt;

use rtnetlink::{IpVersion};
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::rule::RuleAttribute;
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

// FwMark for traffic the does not go through the VPN
const FW_MARK: u32 = 0xFEED_CAFE;

// Table for traffic that does not go through the VPN
const TABLE_ID: u32 = 108;

const IF_VPN: &str = "wg0";

const IF_WAN: &str = "eth0";

fn setup_iptables(vpn_uid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    iptables.delete_chain("mangle", "OUTPUT")?;
    iptables.new_chain("mangle", "OUTPUT")?;

    // Keep loopback for VPN user unmarked
    iptables.append("mangle", "OUTPUT", &format!("-m owner --uid-owner {vpn_uid} -o lo -j RETURN"))?;
    // Mark all other traffic from VPN user
    iptables.append("mangle", "OUTPUT", &format!("-m owner --uid-owner {vpn_uid} -j MARK --set-mark {}", FW_MARK))?;

    Ok(())
}

fn flush_ip_tables() -> Result<(), Box<dyn std::error::Error>> {
    let iptables = iptables::new(false)?;
    iptables.flush_chain("mangle", "OUTPUT")?;
    Ok(())
}

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
#[async_trait]
impl Routing for Router {
    async fn setup(&self) -> Result<(), Error> {
        let (_c, handle, _rx) = rtnetlink::new_connection()?;

        let ifs = handle.link().get().execute()
            .try_collect::<Vec<_>>().await?;

        let vpn_if_index = ifs.iter()
            .find(|i| i.attributes.iter().any(|attr| matches!(attr, LinkAttribute::IfName(name) if name == IF_VPN)))
            .ok_or(Error::General(format!("vpn interface {} not found", IF_VPN)))?
            .header
            .index;

        let wan_if_index = ifs.iter()
            .find(|i| i.attributes.iter().any(|attr| matches!(attr, LinkAttribute::IfName(name) if name == IF_WAN)))
            .ok_or(Error::General(format!("wan interface {} not found", IF_WAN)))?
            .header
            .index;

        // Check if the fwmark rule already exists
        let rules = handle.rule().get(IpVersion::V4).execute().try_collect::<Vec<_>>().await?;
        if rules.into_iter().any(|rule| rule.attributes.iter().any(|a| matches!(a, RuleAttribute::FwMark(fwmark) if *fwmark == FW_MARK))) {
            tracing::info!("fwmark {} already set", FW_MARK);
            return Ok(())
        }

        // Adjust the main routing table so that everything gets routed via the VPN interface
        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(vpn_if_index)
            .build();
        handle.route().add(default_route).execute().await?;
        tracing::debug!("set main table default route to interface {} (index {vpn_if_index})", IF_VPN);

        // Route for TABLE_ID: All traffic goes to the WAN interface (bypasses VPN)
        let no_vpn_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .table_id(TABLE_ID)
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_if_index)
            .build();
        handle.route().add(no_vpn_route).execute().await?;
        tracing::debug!("set table {} default route to interface {} (index {wan_if_index})", TABLE_ID, IF_WAN);

        // Add rule: everything marked with FW_MARK goes via TABLE_ID routing table
        handle.rule()
            .add()
            .fw_mark(FW_MARK)
            .table_id(TABLE_ID)
            .execute()
            .await?;
        tracing::debug!("set fwmark {} routing table", TABLE_ID);

        // This steps marks all traffic from VPN_USER with FW_MARK
        setup_iptables(self.worker.uid).map_err(Error::iptables)?;

        // Generate wg quick content
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            // true to route all traffic
            false,
            // Disable all routing set by wg-quick
            // Set the FwMark on WG's own UDP packets to allow them to go to the Session
            Some([
                "Table = off".to_string(),
                format!("FwMark = {:#X}", FW_MARK)
            ].into_iter().collect())
       );
        // Run wg-quick up
        wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), Error> {
        // Run wg-quick down
        wg_tooling::down().await?;

        // Flush the iptables rules
        flush_ip_tables().map_err(Error::iptables)?;

        // Delete the fwmark routing table rule
        let (_c, handle, _rx) = rtnetlink::new_connection()?;
        let rules = handle.rule().get(IpVersion::V4).execute().try_collect::<Vec<_>>().await?;
        for rule in rules.into_iter().filter(|rule| {
            rule.attributes.iter().any(|a| matches!(a, RuleAttribute::FwMark(fwmark) if fwmark == &FW_MARK)) &&
                rule.attributes.iter().any(|a| matches!(a, RuleAttribute::Table(table) if table == &TABLE_ID))
        }) {
            handle.rule().del(rule).execute().await?;
            tracing::debug!("deleted fwmark {} routing table rule", FW_MARK);
        }

        handle.route().del(rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default().table_id(TABLE_ID).build()).execute().await?;
        tracing::debug!("deleted table {}", TABLE_ID);

        let ifs = handle.link().get().execute()
            .try_collect::<Vec<_>>().await?;

        let wan_if_index = ifs.iter()
            .find(|i| i.attributes.iter().any(|attr| matches!(attr, LinkAttribute::IfName(name) if name == IF_WAN)))
            .ok_or(Error::General(format!("wan interface {} not found", IF_WAN)))?
            .header
            .index;

        let default_route = rtnetlink::RouteMessageBuilder::<Ipv4Addr>::default()
            .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
            .output_interface(wan_if_index)
            .build();
        handle.route().add(default_route).execute().await?;
        tracing::debug!("set main table default route to interface {} (index {wan_if_index})", IF_WAN);

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
            "ip route add {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device
        ),
        None => format!(
            "ip route add {relayer_ip} dev {device}",
            relayer_ip = relayer_ip,
            device = device
        ),
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
    match gateway {
        Some(gw) => format!(
            "ip route del {relayer_ip} via {gateway} dev {device}",
            relayer_ip = relayer_ip,
            gateway = gw,
            device = device,
        ),
        None => format!(
            "ip route del {relayer_ip} dev {device}",
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
