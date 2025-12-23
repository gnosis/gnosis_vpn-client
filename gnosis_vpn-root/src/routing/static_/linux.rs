use async_trait::async_trait;

use crate::routing::Error;
use crate::routing::{Routing, util};

use super::Static;

#[async_trait]
impl Routing for Static {
    async fn setup(&self) -> Result<(), Error> {
        let interface_gateway = util::interface().await?;
        Err(Error::NotImplemented)
    }

    async fn teardown(&self) -> Result<(), Error> {
        Err(Error::NotImplemented)
    }
}

fn pre_up_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        if let Some(ref gateway) = interface.gateway {
            format!(
                "route -n add --host {relayer_ip} {gateway}",
                relayer_ip = relayer_ip,
                gateway = gateway
            )
        } else {
            format!(
                "route -n add -host {relayer_ip} -interface {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route add {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route add {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        format!("route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route del {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route del {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
}
