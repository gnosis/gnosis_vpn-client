//! Interface discovery and the capture thread.
//!
//! The thread is deliberately dumb: it parses frames into observations, records
//! them in per-second aggregates and hands finished batches to the session. It
//! never touches daemon state, never blocks on a consumer and never keeps a
//! packet.

use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gnosis_vpn_lib::traffic::Interface;
use pnet_datalink::{self, Channel, Config, NetworkInterface};

use super::classify::{self, Aggregates};
use super::{PeerAddresses, SessionShared};

/// How often finished aggregates are handed to the session.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// An idle session still publishes one batch per second so consumers can age.
const HEARTBEAT_INTERVAL_MS: u64 = 1000;
/// Bounds how long a blocked read can delay noticing a stop request.
const READ_TIMEOUT: Duration = Duration::from_millis(50);
const START_TIMEOUT: Duration = Duration::from_secs(3);

/// A capture-capable interface plus the platform handle needed to open it.
pub struct Candidate {
    pub descriptor: Interface,
    link: NetworkInterface,
}

/// Lists interfaces that carry public traffic, in stable name order.
///
/// Loopback and tunnel interfaces are excluded: a tunnel is the VPN's own device
/// (or another VPN's), not the interface this view describes. Interfaces without
/// a routable address are excluded because direction cannot be inferred on them.
pub fn discover() -> Vec<Candidate> {
    let default_ip = egress_ip();
    let mut candidates: Vec<Candidate> = pnet_datalink::interfaces()
        .into_iter()
        .filter(|link| !link.is_loopback() && !is_tunnel_name(&link.name))
        .filter_map(|link| {
            let addresses: Vec<IpAddr> = link
                .ips
                .iter()
                .map(|network| network.ip())
                .filter(|ip| is_routable_local(*ip))
                .collect();
            if addresses.is_empty() {
                return None;
            }
            let is_default = default_ip.is_some_and(|ip| addresses.contains(&ip));
            Some(Candidate {
                descriptor: Interface {
                    id: link.name.clone(),
                    name: link.name.clone(),
                    addresses,
                    is_default,
                },
                link,
            })
        })
        .collect();
    candidates.sort_by(|a, b| a.descriptor.name.cmp(&b.descriptor.name));
    candidates
}

/// A tunnel device never describes the public path traffic takes.
fn is_tunnel_name(name: &str) -> bool {
    const TUNNEL_PREFIXES: [&str; 9] = ["utun", "tun", "tap", "wg", "ipsec", "ppp", "gif", "stf", "awdl"];
    TUNNEL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn is_routable_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_loopback() && !v4.is_multicast() && !v4.is_unspecified() && !v4.is_link_local() && !v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            !v6.is_loopback() && !v6.is_multicast() && !v6.is_unspecified() && !(v6.segments()[0] & 0xffc0 == 0xfe80)
        }
    }
}

/// The address the host would use for internet traffic, if any.
///
/// Connecting a datagram socket sends nothing; it only asks the routing table
/// which local address would be used. While a VPN tunnel owns the default route
/// this resolves to the tunnel, which is not a candidate, so no interface is
/// marked as the default in that case.
fn egress_ip() -> Option<IpAddr> {
    for target in ["1.1.1.1:443", "[2606:4700:4700::1111]:443"] {
        let bind = if target.starts_with('[') { "[::]:0" } else { "0.0.0.0:0" };
        let Ok(socket) = UdpSocket::bind(bind) else {
            continue;
        };
        if socket.connect(target).is_err() {
            continue;
        }
        if let Ok(local) = socket.local_addr() {
            let ip = local.ip();
            if !ip.is_unspecified() {
                return Some(ip);
            }
        }
    }
    None
}

/// A running capture. Dropping it stops the thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Stops the thread and waits for it, so no callback outlives the session.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens a capture on `candidate` and returns once packets can arrive.
///
/// A failure here leaves whatever capture was running untouched.
pub fn start(
    candidate: Candidate,
    session: Arc<SessionShared>,
    peers: Arc<PeerAddresses>,
    stop: Arc<AtomicBool>,
) -> Result<Capture, String> {
    let (started_tx, started_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let thread_stop = stop.clone();
    let local: HashSet<IpAddr> = candidate.descriptor.addresses.iter().copied().collect();
    let thread = thread::Builder::new()
        .name("traffic-capture".to_string())
        .spawn(move || run(candidate.link, local, session, peers, thread_stop, started_tx))
        .map_err(|e| format!("Cannot spawn capture thread: {e}"))?;

    match started_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(())) => Ok(Capture {
            stop,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            Err("Capture did not start in time".to_string())
        }
    }
}

fn run(
    link: NetworkInterface,
    local: HashSet<IpAddr>,
    session: Arc<SessionShared>,
    peers: Arc<PeerAddresses>,
    stop: Arc<AtomicBool>,
    started: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let name = link.name.clone();
    let config = Config {
        read_timeout: Some(READ_TIMEOUT),
        promiscuous: false,
        ..Default::default()
    };
    let mut receiver = match pnet_datalink::channel(&link, config) {
        Ok(Channel::Ethernet(_, receiver)) => receiver,
        Ok(_) => {
            let _ = started.send(Err(format!("{name}: unsupported link layer")));
            return;
        }
        Err(e) => {
            let _ = started.send(Err(format!("Cannot capture on {name}: {e}")));
            return;
        }
    };
    if started.send(Ok(())).is_err() {
        return;
    }
    tracing::info!(target: "traffic", interface = %name, "capture started");

    let mut aggregates = Aggregates::default();
    let mut last_flush = Instant::now();
    let mut last_batch_ms = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match receiver.next() {
            Ok(frame) => {
                if let Some(observation) = classify::observe(frame, &local) {
                    let second = session.elapsed_ms() / 1000;
                    let class = peers.classify(observation.target);
                    aggregates.record(second, observation, class);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                session.end(format!("capture on {name} stopped: {e}"));
                tracing::warn!(target: "traffic", interface = %name, error = %e, "capture stopped");
                return;
            }
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            last_flush = Instant::now();
            let elapsed_ms = session.elapsed_ms();
            if !aggregates.is_empty() || elapsed_ms.saturating_sub(last_batch_ms) >= HEARTBEAT_INTERVAL_MS {
                last_batch_ms = elapsed_ms;
                session.push(aggregates.take(), elapsed_ms);
            }
        }
    }
    tracing::info!(target: "traffic", interface = %name, "capture stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_and_loopback_names_are_excluded() {
        for name in ["utun8", "tun0", "tap1", "wg0_gnosisvpn", "ipsec0", "ppp0"] {
            assert!(is_tunnel_name(name), "{name}");
        }
        for name in ["en0", "en7", "wlan0", "wlp3s0", "eth0"] {
            assert!(!is_tunnel_name(name), "{name}");
        }
    }

    #[test]
    fn only_routable_local_addresses_qualify() {
        for ip in [
            "127.0.0.1",
            "224.0.0.1",
            "0.0.0.0",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "ff02::1",
        ] {
            assert!(!is_routable_local(ip.parse().expect("ip")), "{ip}");
        }
        for ip in ["192.168.1.20", "172.20.10.14", "2001:db8::5"] {
            assert!(is_routable_local(ip.parse().expect("ip")), "{ip}");
        }
    }
}
