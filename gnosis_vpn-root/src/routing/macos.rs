//! MacOS specific routing using the pf.
//!
//! Currently only supports setting up WireGuard interface and determining default interface.

use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
// use gnosis_vpn_lib::dirs;
use gnosis_vpn_lib::{event, worker};

use std::net::Ipv4Addr;
// use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use crate::wg_tooling;

use super::{Error, Routing};

pub fn build_firewall_router(worker: worker::Worker, wg_data: event::WireGuardData) -> Result<impl Routing, Error> {
    Ok(Firewall { worker, wg_data })
}

pub fn static_fallback_router(_wg_data: event::WireGuardData, _peer_ips: Vec<Ipv4Addr>) -> impl Routing {
    DisabledFallbackRouter
}

// const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

pub struct Firewall {
    #[allow(dead_code)]
    worker: worker::Worker,
    wg_data: event::WireGuardData,
}

pub struct FallbackRouter {
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

pub struct DisabledFallbackRouter;

impl Firewall {
    pub const ANCHOR_NAME: &str = "gnosisvpn_bypass";
}

#[async_trait]
impl Routing for Firewall {
    /**
     * Refactor logic to use:
     * - pfctl shell command
     */
    #[tracing::instrument(name = "Firewall::setup",level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn setup(&mut self) -> Result<(), Error> {
        if let Err(err) = wg_tooling::down().await {
            tracing::debug!(error = ?err, "wg-quick down before setup failed");
        }
        if let Err(err) = reset_pf_anchor().await {
            tracing::warn!(error = ?err, "pf anchor reset before setup failed");
        }

        let (device, gateway) = interface().await?;
        tracing::info!(%device, ?gateway, "Determined default interface");

        if let Err(err) = cleanup_utun_host_routes().await {
            tracing::warn!(error = ?err, "utun host route cleanup before setup failed");
        }
        if let Some(gateway) = gateway.as_deref() {
            if let Err(err) = cleanup_gateway_host_routes(&device, gateway).await {
                tracing::warn!(error = ?err, "gateway host route cleanup before setup failed");
            }
        }

        // 2. generate wg quick content
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, None);

        // 3. run wg-quick up
        wg_tooling::up(wg_quick_content).await?;

        // 4. setup bypass

        // Enable the firewall, equivalent to the command "pfctl -e":
        tracing::info!("Enabling PF...");
        let _ = Command::new("pfctl")
            .arg("-e")
            .output()
            .await
            .map_err(|e| Error::General(format!("Failed to enable pfctl: {e}")))?;

        tracing::info!("Ensuring anchor link exists in main ruleset...");
        let main_rules = Command::new("pfctl")
            .arg("-sr")
            .output()
            .await
            .map_err(|e| Error::General(format!("Failed to read pf rules: {e}")))?;

        let rules_str = String::from_utf8_lossy(&main_rules.stdout);
        if !rules_str.contains(&format!("anchor \"{}\"", Firewall::ANCHOR_NAME)) {
            tracing::info!("Linking anchor to main ruleset...");

            let mut child = Command::new("pfctl")
                .arg("-f")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(&main_rules.stdout).await?;
                stdin.write_all(b"\n").await?;
                stdin
                    .write_all(format!("anchor \"{}\"\n", Firewall::ANCHOR_NAME).as_bytes())
                    .await?;
            }
            child.wait().await?;
        }

        tracing::info!("Flushing rules for anchor {}...", Firewall::ANCHOR_NAME);
        Command::new("pfctl")
            .args(&["-a", Firewall::ANCHOR_NAME, "-F", "all"])
            .output()
            .await?;

        let gw_ip: std::net::IpAddr = gateway
            .ok_or(Error::General("No gateway found".into()))?
            .as_str()
            .parse()
            .map_err(|e| Error::General(format!("failed to convert gatewat to IpAddr: {e}")))?;

        let utun_device = wg_interface_from_public_key(&self.wg_data.wg.key_pair.public_key).await?;
        let wg_address = parse_wg_ip(&self.wg_data.interface_info.address)?;

        tracing::info!(
            "PF Rule Params: device={}, gateway={:?}, utun_device={}, uid={}, wg_address={}",
            device,
            gw_ip,
            utun_device,
            self.worker.uid,
            wg_address
        );

        let rule_str = build_pf_rule(&device, &utun_device, gw_ip, self.worker.uid, wg_address);

        tracing::info!("Adding rule: {}", rule_str);

        let mut child = Command::new("pfctl")
            .args(&["-a", Firewall::ANCHOR_NAME, "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(rule_str.as_bytes()).await?;
        }

        let output = child.wait_with_output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::General(format!("Failed to add pf rule: {}", stderr)));
        }

        tracing::info!("Bypass rule added successfully via shell.");

        // Debug: Log full configuration
        match Command::new("pfctl").arg("-sr").output().await {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                tracing::info!("--- Main Rules ---\n{}", stdout);
            }
            Err(e) => tracing::warn!("Failed to fetch main rules: {}", e),
        }

        match Command::new("pfctl")
            .args(&["-a", Firewall::ANCHOR_NAME, "-sr"])
            .output()
            .await
        {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                tracing::info!("--- Anchor {} Rules ---\n{}", Firewall::ANCHOR_NAME, stdout);
            }
            Err(e) => tracing::warn!("Failed to fetch anchor rules: {}", e),
        }

        Ok(())
    }

    #[tracing::instrument(name = "Firewall::teardown", level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn teardown(&mut self) -> Result<(), Error> {
        reset_pf_anchor().await?;
        wg_tooling::down().await?;

        Ok(())
    }
}

#[async_trait]
impl Routing for FallbackRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        let interface_gateway = interface().await?;
        let mut extra = self
            .peer_ips
            .iter()
            .map(|ip| pre_up_routing(ip, interface_gateway.clone()))
            .collect::<Vec<String>>();
        extra.extend(
            self.peer_ips
                .iter()
                .map(|ip| pre_down_routing(ip, interface_gateway.clone()))
                .collect::<Vec<String>>(),
        );
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

    async fn teardown(&mut self) -> Result<(), Error> {
        wg_tooling::down().await?;
        Ok(())
    }
}

#[async_trait]
impl Routing for DisabledFallbackRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        Err(Error::General("Static routing is disabled on macOS".to_string()))
    }

    async fn teardown(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

fn build_pf_rule(
    device: &str,
    utun_device: &str,
    gateway: std::net::IpAddr,
    uid: u32,
    wg_address: std::net::IpAddr,
) -> String {
    [
        "scrub all fragment reassemble".to_string(),
        format!("nat on {device} inet from {wg_address} to any -> ({device})"),
        "pass quick on lo0 all flags any keep state".to_string(),
        format!("pass out quick on {device} inet proto udp from any port = 68 to 255.255.255.255 port = 67 no state"),
        format!("pass in quick on {device} inet proto udp from any port = 67 to any port = 68 no state"),
        format!("pass out quick route-to ({device} {gateway}) inet all user {uid} keep state"),
        format!("pass out quick on {device} proto udp from any port 67:68 to any port 67:68 keep state"),
        format!("pass quick on {utun_device} all user != {uid}"),
        format!("block drop out on {device} all"),
    ]
    .join("\n")
        + "\n"
}

async fn reset_pf_anchor() -> Result<(), Error> {
    Command::new("pfctl")
        .args(&["-a", Firewall::ANCHOR_NAME, "-F", "all"])
        .output()
        .await?;

    let main_rules = Command::new("pfctl")
        .arg("-sr")
        .output()
        .await
        .map_err(|e| Error::General(format!("Failed to read pf rules: {e}")))?;

    let rules_str = String::from_utf8_lossy(&main_rules.stdout);
    if let Some(new_rules) = strip_anchor_link(&rules_str, Firewall::ANCHOR_NAME) {
        let mut child = Command::new("pfctl")
            .arg("-f")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(new_rules.as_bytes()).await?;
        }
        child.wait().await?;
    }

    Ok(())
}

fn strip_anchor_link(rules: &str, anchor: &str) -> Option<String> {
    if !rules.contains(&format!("anchor \"{}\"", anchor)) {
        return None;
    }

    Some(
        rules
            .lines()
            .filter(|line| !line.contains(&format!("anchor \"{}\"", anchor)))
            .collect::<Vec<_>>()
            .join("\n"),
    )
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

fn parse_wg_ip(address: &str) -> Result<std::net::IpAddr, Error> {
    let ip_str = address.split('/').next().unwrap_or(address);
    ip_str
        .parse()
        .map_err(|e| Error::General(format!("failed to parse wg address {address}: {e}")))
}

async fn cleanup_utun_host_routes() -> Result<(), Error> {
    let output = Command::new("netstat")
        .arg("-rn")
        .arg("-f")
        .arg("inet")
        .run_stdout()
        .await?;

    let destinations = parse_utun_host_routes(&output);
    for destination in destinations {
        let _ = Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-host")
            .arg(destination)
            .run_stdout()
            .await;
    }

    Ok(())
}

fn parse_utun_host_routes(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                return None;
            }
            let destination = parts[0];
            let flags = parts[2];
            let netif = parts[3];
            if flags.contains('H') && netif.starts_with("utun") {
                Some(destination.to_string())
            } else {
                None
            }
        })
        .collect()
}

async fn cleanup_gateway_host_routes(device: &str, gateway: &str) -> Result<(), Error> {
    let output = Command::new("netstat")
        .arg("-rn")
        .arg("-f")
        .arg("inet")
        .run_stdout()
        .await?;

    let destinations = parse_gateway_host_routes(&output, device, gateway);
    for destination in destinations {
        let _ = Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-host")
            .arg(destination)
            .run_stdout()
            .await;
    }

    Ok(())
}

fn parse_gateway_host_routes(output: &str, device: &str, gateway: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                return None;
            }
            let destination = parts[0];
            let route_gateway = parts[1];
            let flags = parts[2];
            let netif = parts[3];
            if flags.contains('H') && flags.contains('G') && netif == device && route_gateway == gateway {
                Some(destination.to_string())
            } else {
                None
            }
        })
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
    fn builds_pf_rule_for_user_on_interface() {
        let rule = super::build_pf_rule(
            "en0",
            "utun10",
            "192.168.88.1".parse().expect("gateway"),
            499,
            "10.128.0.115".parse().expect("wg_address"),
        );

        assert_eq!(
            rule,
            "scrub all fragment reassemble\nnat on en0 inet from 10.128.0.115 to any -> (en0)\npass quick on lo0 all flags any keep state\npass out quick on en0 inet proto udp from any port = 68 to 255.255.255.255 port = 67 no state\npass in quick on en0 inet proto udp from any port = 67 to any port = 68 no state\npass out quick route-to (en0 192.168.88.1) inet all user 499 keep state\npass out quick on en0 proto udp from any port 67:68 to any port 67:68 keep state\npass quick on utun10 all user != 499\nblock drop out on en0 all\n"
        );
    }

    #[test]
    fn strips_anchor_link_from_rules() {
        let rules = "block drop all\nanchor \"gnosisvpn_bypass\"\npass out all";

        let stripped = super::strip_anchor_link(rules, "gnosisvpn_bypass");

        assert_eq!(stripped, Some("block drop all\npass out all".to_string()));
    }

    #[test]
    fn parses_utun_host_routes() {
        let output = "Destination        Gateway            Flags               Netif Expire\n10.0.0.1          10.0.0.1           UH                 utun8\n10.0.0.2          10.0.0.2           UHS                utun9\n0/1               utun8              UScg               utun8\n";

        let destinations = super::parse_utun_host_routes(output);

        assert_eq!(destinations, vec!["10.0.0.1", "10.0.0.2"]);
    }

    #[test]
    fn parses_gateway_host_routes() {
        let output = "Destination        Gateway            Flags               Netif Expire\n185.9.1.81        192.168.88.1      UGHS               en0\n10.0.0.1          10.0.0.1           UH                 utun8\n";

        let destinations = super::parse_gateway_host_routes(output, "en0", "192.168.88.1");

        assert_eq!(destinations, vec!["185.9.1.81"]);
    }
}
