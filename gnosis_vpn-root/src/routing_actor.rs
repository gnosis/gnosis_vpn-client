use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::killswitch::Firewall;
use gnosis_vpn_lib::shell_command_ext::Logs;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::routing::{self, Routing};

pub enum Msg {
    SetupRouting {
        state_home: PathBuf,
        wg_data: Box<event::WireGuardData>,
        peer_ips: Vec<Ipv4Addr>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    TeardownRouting {
        reply: oneshot::Sender<()>,
    },
    SetAllowedIps {
        ips: Vec<IpAddr>,
        interface: String,
        lan_lockdown: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    DisableKillswitch,
    /// Re-apply the last successfully applied killswitch policy (e.g. after a network change).
    ReapplyKillswitch,
}

struct AppliedPolicy {
    interface: String,
    ips: Vec<IpAddr>,
    lan_lockdown: bool,
}

/// Source of truth for routing state: owns the static router and the firewall.
/// All routing and killswitch mutations are serialized through this actor's
/// message queue, so setup, teardown and policy changes cannot interleave.
struct Actor {
    firewall: Firewall,
    router: Option<Box<dyn Routing + Send>>,
    applied_policy: Option<AppliedPolicy>,
}

impl Actor {
    fn new() -> Result<Self, String> {
        Ok(Actor {
            firewall: Firewall::new().map_err(|e| e.to_string())?,
            router: None,
            applied_policy: None,
        })
    }

    // Firewall ops (netlink / PF ioctls) are synchronous but complete in < 1 ms,
    // so they run inline. Routing setup/teardown (wg-quick, route changes) is
    // genuinely async and may take seconds — queued messages simply wait,
    // which is exactly the serialization we want.
    async fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::SetupRouting {
                state_home,
                wg_data,
                peer_ips,
                reply,
            } => {
                let result = self.setup_routing(state_home, *wg_data, peer_ips).await;
                let _ = reply.send(result);
            }
            Msg::TeardownRouting { reply } => {
                self.teardown_routing().await;
                let _ = reply.send(());
            }
            Msg::SetAllowedIps {
                ips,
                interface,
                lan_lockdown,
                reply,
            } => {
                let result = self.apply_policy(interface, ips, lan_lockdown);
                let _ = reply.send(result);
            }
            Msg::DisableKillswitch => {
                self.applied_policy = None;
                if let Err(error) = self.firewall.reset_policy() {
                    tracing::warn!(?error, "failed to disable killswitch on disconnect");
                }
            }
            Msg::ReapplyKillswitch => self.reapply_policy(),
        }
    }

    async fn setup_routing(
        &mut self,
        state_home: PathBuf,
        wg_data: event::WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
    ) -> Result<String, String> {
        // ensure clean slate
        self.teardown_routing().await;

        let mut router = match routing::static_router(state_home, wg_data, peer_ips) {
            Ok(router) => router,
            Err(error) => {
                tracing::error!(?error, "failed to build static router");
                return Err(error.to_string());
            }
        };
        let res_setup = router.setup().await;
        // store the router even on setup error so partial state can be torn down
        self.router = Some(Box::new(router));
        match res_setup {
            Ok(interface_name) => {
                tracing::info!("static routing setup successfully");
                Ok(interface_name)
            }
            Err(error) => {
                tracing::error!(?error, "static routing setup error");
                self.teardown_routing().await;
                Err(error.to_string())
            }
        }
    }

    async fn teardown_routing(&mut self) {
        if let Some(ref mut router) = self.router {
            router.teardown(Logs::Print).await;
        }
        self.router = None;
    }

    fn apply_policy(&mut self, interface: String, ips: Vec<IpAddr>, lan_lockdown: bool) -> Result<(), String> {
        let result = self
            .firewall
            .apply_policy(&interface, &ips, lan_lockdown)
            .map_err(|e| e.to_string());
        match result {
            Ok(()) => {
                self.applied_policy = Some(AppliedPolicy {
                    interface,
                    ips,
                    lan_lockdown,
                });
                Ok(())
            }
            Err(error) => {
                tracing::error!(?error, "failed to apply killswitch policy");
                Err(error)
            }
        }
    }

    fn reapply_policy(&mut self) {
        let Some(policy) = &self.applied_policy else {
            return;
        };
        tracing::info!("re-applying killswitch after network change");
        if let Err(error) = self
            .firewall
            .apply_policy(&policy.interface, &policy.ips, policy.lan_lockdown)
        {
            tracing::warn!(?error, "failed to re-apply killswitch after network change");
        }
    }

    async fn teardown(&mut self) {
        self.teardown_routing().await;
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
                Some(msg) => actor.handle(msg).await,
                None => {
                    tracing::info!("routing actor channel closed");
                    break;
                }
            }
        }
    }
    actor.teardown().await;
}
