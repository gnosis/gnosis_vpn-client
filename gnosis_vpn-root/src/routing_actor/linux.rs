use std::net::IpAddr;

use crate::routing::killswitch::Firewall;

pub enum Msg {
    SetAllowedIps(Vec<IpAddr>),
}

pub(super) struct Actor {
    firewall: Firewall,
}

impl Actor {
    pub(super) fn new() -> Self {
        Actor {
            firewall: Firewall::new(),
        }
    }

    pub(super) fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::SetAllowedIps(ips) => {
                if let Err(error) = self.firewall.apply_policy(&ips) {
                    tracing::error!(?error, "failed to apply killswitch policy");
                }
            }
        }
    }

    pub(super) fn teardown(&mut self) {
        if let Err(error) = self.firewall.reset_policy() {
            tracing::warn!(?error, "failed to reset killswitch policy on shutdown");
        }
    }
}
