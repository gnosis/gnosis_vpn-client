use std::net::IpAddr;

use gnosis_vpn_lib::killswitch::Firewall;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub enum Msg {
    SetAllowedIps {
        ips: Vec<IpAddr>,
        interface: String,
        lan_lockdown: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    DisableKillswitch,
}

struct Actor {
    firewall: Firewall,
}

impl Actor {
    fn new() -> Result<Self, String> {
        Ok(Actor {
            firewall: Firewall::new().map_err(|e| e.to_string())?,
        })
    }

    // Firewall ops (netlink / PF ioctls) are synchronous but complete in < 1 ms
    // and only fire at connect/disconnect, so blocking a worker thread briefly is
    // harmless here — the complexity of spawn_blocking + Arc<Mutex<>> isn't worth it.
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::SetAllowedIps {
                ips,
                interface,
                lan_lockdown,
                reply,
            } => {
                let result = self
                    .firewall
                    .apply_policy(&interface, &ips, lan_lockdown)
                    .map_err(|e| e.to_string());
                if let Err(ref error) = result {
                    tracing::error!(?error, "failed to apply killswitch policy");
                }
                let _ = reply.send(result);
            }
            Msg::DisableKillswitch => {
                if let Err(error) = self.firewall.reset_policy() {
                    tracing::warn!(?error, "failed to disable killswitch on disconnect");
                }
            }
        }
    }

    fn teardown(&mut self) {
        if let Err(error) = self.firewall.reset_policy() {
            tracing::warn!(?error, "failed to reset killswitch policy on shutdown");
        }
    }
}

pub fn start(cancel: CancellationToken) -> Result<(mpsc::Sender<Msg>, tokio::task::JoinHandle<()>), String> {
    let actor = Actor::new()?;
    let (sender, receiver) = mpsc::channel(32);
    let handle = tokio::spawn(run(actor, receiver, cancel));
    Ok((sender, handle))
}

async fn run(mut actor: Actor, mut receiver: mpsc::Receiver<Msg>, cancel: CancellationToken) {
    tracing::info!("routing actor started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("routing actor stopping");
                break;
            }
            msg = receiver.recv() => match msg {
                Some(msg) => actor.handle(msg),
                None => {
                    tracing::info!("routing actor channel closed");
                    break;
                }
            }
        }
    }
    actor.teardown();
}
