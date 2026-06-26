//! Routing and killswitch actor.
//!
//! Serialises all routing and firewall mutations through a single message queue so that
//! setup, teardown, and policy changes cannot interleave.
//!
//! The killswitch allowlist has two tiers:
//! * **Static floor** (`AppliedPolicy::ips`) — set once at `KillswitchLockdown` time
//!   (blokli IPs + peers alive at initial connection). Overwritten on reconnect, cleared
//!   only on explicit disconnect, preserved as-is during a worker crash so the firewall
//!   stays active until the next successful connection.
//! * **Dynamic delta** (`Actor::active_bypass`) — peers discovered after initial
//!   connection. Added and removed by `update_peer_ips`; reset to empty on routing
//!   teardown. The firewall sees `floor ∪ delta`, so the floor is always allowed even
//!   when the delta shrinks to zero.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::killswitch::Firewall;
use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::wireguard;
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
    /// Ask whether a reconnect should be triggered given the latest network event burst.
    /// `removed_link` is the name of any interface removed during the burst, if any.
    /// Replies `false` when no routing is active (nothing to reconnect) or when
    /// the events were caused by our own route mutations or unrelated route churn.
    NetworkChanged {
        removed_link: Option<String>,
        reply: oneshot::Sender<bool>,
    },
    /// Fire-and-forget: update the peer-IP bypass routes and killswitch allowlist.
    /// Sent periodically from Core with a snapshot of announced peer IPv4 addresses.
    /// The actor applies hysteresis and diffs against its current active set.
    UpdatePeerIps {
        peer_ips: Vec<Ipv4Addr>,
    },
}

const PEER_IP_HYSTERESIS_SECS: u64 = 300;

struct AppliedPolicy {
    interface: String,
    /// Static floor: blokli IPs + peers alive at initial connection.
    /// Never updated by peer refreshes; overwritten only when a new lockdown fires.
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
    /// Timestamp of the last `update_peer_ips` observation per IP.
    /// An IP is retained in the allowlist for `PEER_IP_HYSTERESIS_SECS` after last observation.
    peer_ip_last_seen: std::collections::HashMap<Ipv4Addr, Instant>,
    /// Dynamic delta above the static floor: peers discovered after initial connection.
    /// Diffed and reconciled by `update_peer_ips`; reset to empty on routing teardown.
    active_bypass: HashSet<Ipv4Addr>,
    /// Resolved WireGuard interface name (e.g. "utun8" on macOS, "wg0_gnosisvpn" on Linux).
    /// Populated after a successful routing setup; cleared on teardown.
    wg_interface_name: Option<String>,
}

impl Actor {
    fn new() -> Result<Self, String> {
        Ok(Actor {
            firewall: Firewall::new().map_err(|e| e.to_string())?,
            router: None,
            applied_policy: None,
            peer_ip_last_seen: std::collections::HashMap::new(),
            active_bypass: HashSet::new(),
            wg_interface_name: None,
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
            Msg::NetworkChanged { removed_link, reply } => {
                let _ = reply.send(self.should_reconnect(removed_link).await);
            }
            Msg::UpdatePeerIps { peer_ips } => {
                self.update_peer_ips(peer_ips).await;
            }
        }
    }

    async fn should_reconnect(&mut self, removed_link: Option<String>) -> bool {
        // On macOS the WireGuard interface is a dynamic utunN, not the config name.
        // Use the resolved name so comparisons and kernel lookups target the real interface.
        let wg_iface = self
            .wg_interface_name
            .as_deref()
            .unwrap_or(wireguard::WG_INTERFACE)
            .to_string();

        let Some(router) = &mut self.router else {
            tracing::debug!("should_reconnect: no active router, skipping");
            return false;
        };

        tracing::debug!(removed_link = ?removed_link, wg_iface = %wg_iface, "should_reconnect: evaluating");

        // Tunnel gone — reconnect regardless of WAN state.
        // Planned teardown can't reach here: TeardownRouting is awaited before
        // NetworkChanged is dispatched, so self.router is None by then.
        if removed_link.as_deref() == Some(&wg_iface) {
            tracing::info!("WireGuard device removed — reconnect needed");
            return true;
        }

        // On macOS, when a utunN interface is deleted, RTM_IFINFO fires but
        // if_indextoname already fails, so we emit LinkRemoved with a synthetic
        // "if#N" name. Check whether the resolved WG interface still exists.
        #[cfg(target_os = "macos")]
        if removed_link.as_ref().map_or(false, |n| n.starts_with("if#")) {
            let wg_gone = std::ffi::CString::new(wg_iface.as_str())
                .map(|name| unsafe { libc::if_nametoindex(name.as_ptr()) } == 0)
                .unwrap_or(false);
            if wg_gone {
                tracing::info!("WireGuard interface gone (confirmed via if_nametoindex) — reconnect needed");
                return true;
            }
        }

        // Only reconnect if the WAN actually changed; our own route mutations
        // also emit events, so checking WAN breaks the reconnect feedback loop.
        let wan_result = router.wan_changed().await;
        tracing::debug!(wan_result = ?wan_result, "should_reconnect: WAN changed check result");
        match wan_result {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(?error, "failed to query WAN default route, assuming network change");
                true
            }
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
                self.wg_interface_name = Some(interface_name.clone());
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
            for ip in self.active_bypass.drain().collect::<Vec<_>>() {
                if let Err(e) = router.remove_peer_bypass_route(ip).await {
                    tracing::warn!(error = %e, peer_ip = %ip, "failed to remove dynamic bypass route on teardown");
                }
            }
            router.teardown(Logs::Print).await;
        }
        self.router = None;
        self.wg_interface_name = None;
        self.peer_ip_last_seen.clear();
        self.active_bypass.clear();
    }

    async fn update_peer_ips(&mut self, peer_ips: Vec<Ipv4Addr>) {
        let now = Instant::now();
        for ip in &peer_ips {
            self.peer_ip_last_seen.insert(*ip, now);
        }
        self.peer_ip_last_seen
            .retain(|_, t| now.duration_since(*t) < Duration::from_secs(PEER_IP_HYSTERESIS_SECS));

        let alive: HashSet<Ipv4Addr> = self.peer_ip_last_seen.keys().copied().collect();

        if alive == self.active_bypass {
            return;
        }

        if let Some(ref mut router) = self.router {
            for ip in alive.difference(&self.active_bypass).copied().collect::<Vec<_>>() {
                if let Err(e) = router.add_peer_bypass_route(ip).await {
                    tracing::warn!(error = %e, peer_ip = %ip, "failed to add dynamic peer bypass route");
                }
            }
            for ip in self.active_bypass.difference(&alive).copied().collect::<Vec<_>>() {
                if let Err(e) = router.remove_peer_bypass_route(ip).await {
                    tracing::warn!(error = %e, peer_ip = %ip, "failed to remove dynamic peer bypass route");
                }
            }
        }

        self.active_bypass = alive.clone();

        // Union the static floor (policy.ips) with the dynamic delta (alive) so blokli
        // and the initial peer snapshot stay allowed even when alive is empty.
        if let Some(ref policy) = self.applied_policy {
            let combined: Vec<IpAddr> = policy
                .ips
                .iter()
                .copied()
                .chain(alive.iter().map(|ip| IpAddr::V4(*ip)))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if let Err(e) = self
                .firewall
                .apply_policy(&policy.interface, &combined, policy.lan_lockdown)
            {
                tracing::warn!(error = %e, "failed to refresh killswitch after peer allowlist update");
            } else {
                tracing::debug!(count = combined.len(), "killswitch peer allowlist refreshed");
            }
        }
    }

    fn apply_policy(&mut self, interface: String, ips: Vec<IpAddr>, lan_lockdown: bool) -> Result<(), String> {
        let ips: Vec<IpAddr> = ips
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
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
        let combined: Vec<IpAddr> = policy
            .ips
            .iter()
            .copied()
            .chain(self.active_bypass.iter().map(|ip| IpAddr::V4(*ip)))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if let Err(error) = self
            .firewall
            .reapply_policy(&policy.interface, &combined, policy.lan_lockdown)
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
