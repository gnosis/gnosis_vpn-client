use std::net::IpAddr;

use tokio::sync::oneshot;

use crate::routing::killswitch::Firewall;

pub enum Msg {
    SetAllowedIps {
        ips: Vec<IpAddr>,
        reply: oneshot::Sender<Result<(), String>>,
    },
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
            Msg::SetAllowedIps { ips, reply } => {
                let result = self.firewall.apply_policy(&ips).map_err(|e| e.to_string());
                if let Err(ref error) = result {
                    tracing::error!(?error, "failed to apply killswitch policy");
                }
                let _ = reply.send(result);
            }
        }
    }

    pub(super) fn teardown(&mut self) {
        if let Err(error) = self.firewall.reset_policy() {
            tracing::warn!(?error, "failed to reset killswitch policy on shutdown");
        }
    }
}
