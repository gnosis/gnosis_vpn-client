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
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use gnosis_vpn_lib::killswitch::Firewall;
use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::wireguard;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::device_monitor::{self, NetworkEvent};
use crate::routing::{self, Routing};

const DEBOUNCE_SETTLE: Duration = Duration::from_millis(250);
const DEBOUNCE_MAX: Duration = Duration::from_secs(1);

pub enum Msg {
    SetupRouting {
        interface_address: String,
        mtu: u32,
        dns: Option<String>,
        peer_ips: Vec<Ipv4Addr>,
        reply: oneshot::Sender<Result<(String, OwnedFd), String>>,
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
    /// Fire-and-forget: update the peer-IP bypass routes and killswitch allowlist.
    /// Sent periodically from Core with a snapshot of announced peer IPv4 addresses.
    /// The actor applies hysteresis and diffs against its current active set.
    UpdatePeerIps {
        peer_ips: Vec<Ipv4Addr>,
    },
}

/// Returned by `Actor::handle` to tell `run` whether to start or stop the device monitor.
enum MonitorAction {
    Start,
    Stop,
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
    async fn handle(&mut self, msg: Msg) -> Option<MonitorAction> {
        match msg {
            Msg::SetupRouting {
                interface_address,
                mtu,
                dns,
                peer_ips,
                reply,
            } => {
                let result = self.setup_routing(interface_address, mtu, dns, peer_ips).await;
                let _ = reply.send(result);
                None
            }
            Msg::TeardownRouting { reply } => {
                self.teardown_routing().await;
                let _ = reply.send(());
                None
            }
            Msg::SetAllowedIps {
                ips,
                interface,
                lan_lockdown,
                reply,
            } => {
                let result = self.apply_policy(interface, ips, lan_lockdown);
                let start_monitor = result.is_ok();
                let _ = reply.send(result);
                if start_monitor {
                    Some(MonitorAction::Start)
                } else {
                    None
                }
            }
            Msg::DisableKillswitch => {
                self.applied_policy = None;
                if let Err(error) = self.firewall.reset_policy() {
                    tracing::warn!(?error, "failed to disable killswitch on disconnect");
                }
                Some(MonitorAction::Stop)
            }
            Msg::UpdatePeerIps { peer_ips } => {
                self.update_peer_ips(peer_ips).await;
                None
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
        if removed_link.as_ref().is_some_and(|n| n.starts_with("if#")) {
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
        interface_address: String,
        mtu: u32,
        dns: Option<String>,
        peer_ips: Vec<Ipv4Addr>,
    ) -> Result<(String, OwnedFd), String> {
        // ensure clean slate
        self.teardown_routing().await;

        let mut router = match routing::static_router(interface_address, mtu, dns, peer_ips) {
            Ok(router) => router,
            Err(error) => {
                tracing::error!(?error, "failed to build static router");
                return Err(error.to_string());
            }
        };
        let res_setup = router.setup().await;
        // Duplicate the TUN fd with close-on-exec while the router still owns the
        // original. The duplicate can then cross the actor channel as an OwnedFd.
        let tun_fd = router
            .tun_fd()
            .map(|fd| rustix::io::fcntl_dupfd_cloexec(fd, 0))
            .transpose();
        // store the router even on setup error so partial state can be torn down
        self.router = Some(Box::new(router));
        match res_setup {
            Ok(interface_name) => match tun_fd {
                Ok(Some(fd)) => {
                    self.wg_interface_name = Some(interface_name.clone());
                    tracing::info!("static routing setup successfully");
                    Ok((interface_name, fd))
                }
                Ok(None) => {
                    tracing::error!("routing setup reported success but produced no TUN fd");
                    self.teardown_routing().await;
                    Err("routing setup produced no TUN fd".to_string())
                }
                Err(error) => {
                    tracing::error!(?error, "failed to duplicate TUN fd");
                    self.teardown_routing().await;
                    Err(format!("failed to duplicate TUN fd: {error}"))
                }
            },
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
                tracing::warn!(error = %e, interface = %policy.interface, "failed to refresh killswitch after peer allowlist update");
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

pub fn start(
    cancel: CancellationToken,
    reconnect_tx: mpsc::Sender<()>,
) -> Result<(mpsc::Sender<Msg>, tokio::task::JoinHandle<()>), String> {
    let actor = Actor::new()?;
    let (sender, receiver) = mpsc::channel(32);
    let handle = tokio::spawn(run(actor, receiver, cancel, reconnect_tx));
    Ok((sender, handle))
}

async fn run(
    mut actor: Actor,
    mut receiver: mpsc::Receiver<Msg>,
    cancel: CancellationToken,
    reconnect_tx: mpsc::Sender<()>,
) {
    tracing::info!("routing actor started");

    let (network_tx, mut network_rx) = mpsc::channel::<NetworkEvent>(32);
    let mut monitor_cancel: Option<CancellationToken> = None;

    let debounce = time::sleep(Duration::MAX);
    tokio::pin!(debounce);
    let mut debounce_pending = false;
    let mut debounce_started: Option<time::Instant> = None;
    let mut removed_link: Option<String> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("routing actor stopping");
                break;
            }
            msg = receiver.recv() => {
                match msg {
                    Some(msg) => {
                        match actor.handle(msg).await {
                            Some(MonitorAction::Start) => {
                                if monitor_cancel.is_none() {
                                    match device_monitor::start(network_tx.clone()) {
                                        Ok((cancel_dm, _handle)) => {
                                            tracing::debug!("device monitor started");
                                            monitor_cancel = Some(cancel_dm);
                                        }
                                        Err(error) => tracing::error!(?error, "failed to start device monitor"),
                                    }
                                }
                            }
                            Some(MonitorAction::Stop) => {
                                if let Some(c) = monitor_cancel.take() {
                                    c.cancel();
                                    tracing::debug!("device monitor stopped");
                                }
                                debounce_pending = false;
                                debounce_started = None;
                                removed_link = None;
                            }
                            None => {}
                        }
                    }
                    None => {
                        tracing::info!("routing actor channel closed");
                        break;
                    }
                }
            }
            event = network_rx.recv() => {
                if let Some(event) = event {
                    log_network_event(&event);
                    if let NetworkEvent::LinkRemoved { name, .. } | NetworkEvent::AddressRemoved { name, .. } = &event {
                        removed_link = Some(name.clone());
                    }
                    let burst_started = *debounce_started.get_or_insert_with(time::Instant::now);
                    let deadline = burst_started + DEBOUNCE_MAX;
                    let settle = time::Instant::now() + DEBOUNCE_SETTLE;
                    debounce.as_mut().reset(settle.min(deadline));
                    debounce_pending = true;
                }
            }
            _ = debounce.as_mut(), if debounce_pending => {
                debounce_pending = false;
                debounce_started = None;
                tracing::debug!(removed_link = ?removed_link, "network burst settled");
                actor.reapply_policy();
                if actor.should_reconnect(removed_link.take()).await {
                    tracing::info!("network changed — notifying daemon to reconnect");
                    let _ = reconnect_tx.send(()).await;
                } else {
                    tracing::debug!("network event burst, no reconnect needed");
                }
            }
        }
    }

    if let Some(c) = monitor_cancel.take() {
        c.cancel();
    }
    actor.teardown().await;
}

fn log_network_event(event: &NetworkEvent) {
    match event {
        NetworkEvent::LinkChanged { index, name } => tracing::info!(index, name, "network link changed"),
        NetworkEvent::LinkRemoved { index, name } => tracing::info!(index, name, "network link removed"),
        NetworkEvent::AddressAdded { index, name } => tracing::info!(index, name, "network address added"),
        NetworkEvent::AddressRemoved { index, name } => tracing::info!(index, name, "network address removed"),
        NetworkEvent::RouteAdded => tracing::info!("route added"),
        NetworkEvent::RouteRemoved => tracing::info!("route removed"),
        #[cfg(target_os = "macos")]
        NetworkEvent::RouteChanged => tracing::info!("route changed"),
    }
}
