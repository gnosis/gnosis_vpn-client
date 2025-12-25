use tokio::process::Command;

use std::net::Ipv4Addr;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;

use crate::routing::Error;

pub fn pre_up_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
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

pub fn post_down_routing(relayer_ip: &Ipv4Addr, (device, gateway): (String, Option<String>)) -> String {
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

pub async fn interface() -> Result<(String, Option<String>), Error> {
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
        assert_eq!(gateway, Some("192.168.101.202".to_string()));
        Ok(())
    }
}
