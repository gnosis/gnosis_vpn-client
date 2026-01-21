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
    Ok(Firewall {
        worker,
        wg_data,
    })
}

pub fn static_fallback_router(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> impl Routing {
    FallbackRouter { wg_data, peer_ips }
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
        // 1. determine interface (Moved before WG setup to get physical interface)
        let (device, gateway) = interface().await?;
        tracing::info!(%device, ?gateway, "Determined default interface");

        // 2. generate wg quick content
        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, true, None);

        // 3. run wg-quick up
        wg_tooling::up(wg_quick_content).await?;

        // 4. setup bypass

        let gw_ip: std::net::IpAddr = gateway
            .ok_or(Error::General("No gateway found".into()))?
            .as_str()
            .parse()
            .map_err(|e| Error::General(format!("failed to convert gatewat to IpAddr: {e}")))?;

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
                stdin.write_all(format!("anchor \"{}\"\n", Firewall::ANCHOR_NAME).as_bytes()).await?;
             }
             child.wait().await?;
        }

        tracing::info!("Flushing rules for anchor {}...", Firewall::ANCHOR_NAME);
        Command::new("pfctl")
            .args(&["-a", Firewall::ANCHOR_NAME, "-F", "all"])
            .output()
            .await?;

        // ... Add rule ...
        tracing::info!(
             "PF Rule Params: device={}, gateway={:?}, uid={}",
             device,
             gw_ip,
             self.worker.uid
        );
        
        let rule_str = format!(
            "pass out quick route-to ({device} {gateway}) inet from any to ! 127.0.0.0/8 user {uid}\n",
            device = device,
            gateway = gw_ip,
            uid = self.worker.uid
        );
        
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
        // 1. remove pf anchor rules
        Command::new("pfctl")
            .args(&["-a", Firewall::ANCHOR_NAME, "-F", "all"])
            .output()
            .await?;
            
        // 2. Optionally remove anchor from main ruleset?
        // The crate `remove_anchor` did `pf_change_rule` to remove it from main ruleset.
        // We probably should cleanup.
        // (pfctl -sr | grep -v 'anchor "gnosisvpn_bypass"') | pfctl -f -
        
        let main_rules = Command::new("pfctl")
            .arg("-sr")
            .output()
            .await
            .map_err(|e| Error::General(format!("Failed to read pf rules: {e}")))?;
            
        let rules_str = String::from_utf8_lossy(&main_rules.stdout);
        if rules_str.contains(&format!("anchor \"{}\"", Firewall::ANCHOR_NAME)) {
             tracing::info!("Removing anchor link from main ruleset...");
             let new_rules = rules_str
                .lines()
                .filter(|l| !l.contains(&format!("anchor \"{}\"", Firewall::ANCHOR_NAME)))
                .collect::<Vec<_>>()
                .join("\n");
                
             let mut child = Command::new("pfctl")
                .arg("-f")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()?;
             
             if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(new_rules.as_bytes()).await?;
                // Ensure newline at end if needed
                if !new_rules.ends_with('\n') {
                    stdin.write_all(b"\n").await?;
                }
             }
             child.wait().await?;
        }

        // 3. run wg-quick down
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
