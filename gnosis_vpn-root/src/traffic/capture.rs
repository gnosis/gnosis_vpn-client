//! Interface discovery and the capture thread, which never touches daemon state or keeps a packet.

use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gnosis_vpn_lib::traffic::Interface;
use pnet_datalink::{self, Channel, Config, NetworkInterface};

use super::SessionShared;
use super::classify::{self, Aggregates, Host, Side};

/// How often finished aggregates are handed to the session.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// An idle session still publishes one batch per second so consumers can age.
const HEARTBEAT_INTERVAL_MS: u64 = 1000;
/// Bounds how long a blocked read can delay noticing a stop request.
const READ_TIMEOUT: Duration = Duration::from_millis(50);
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// The 4 KiB default loses a third of a fast download; 512 KiB is the macOS BPF ceiling.
const READ_BUFFER_SIZE: usize = 512 * 1024;

/// A capture-capable interface plus the platform handle needed to open it.
pub struct Candidate {
    pub descriptor: Interface,
    link: NetworkInterface,
    side: Side,
}

/// Public interfaces with a routable address; tunnels are excluded since they are not the public path.
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
                side: Side::Wire,
            })
        })
        .collect();
    candidates.sort_by(|a, b| a.descriptor.name.cmp(&b.descriptor.name));
    candidates
}

/// The VPN tunnel device owning the default route; shows traffic before the VPN encrypts it.
pub fn default_tunnel() -> Option<Candidate> {
    let egress = egress_ip()?;
    pnet_datalink::interfaces().into_iter().find_map(|link| {
        let addresses: Vec<IpAddr> = link
            .ips
            .iter()
            .map(|network| network.ip())
            .filter(|ip| is_routable_local(*ip))
            .collect();
        if !carries_egress(&link.name, &addresses, egress) {
            return None;
        }
        Some(Candidate {
            descriptor: Interface {
                id: link.name.clone(),
                name: link.name.clone(),
                addresses,
                is_default: true,
            },
            link,
            side: Side::Tunnel,
        })
    })
}

fn carries_egress(name: &str, addresses: &[IpAddr], egress: IpAddr) -> bool {
    is_vpn_tunnel_name(name) && addresses.contains(&egress)
}

/// Only devices that carry bare IP packets; TAP, IPsec and PPP frame them differently.
fn is_vpn_tunnel_name(name: &str) -> bool {
    const VPN_TUNNEL_PREFIXES: [&str; 3] = ["utun", "tun", "wg"];
    VPN_TUNNEL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// A tunnel device never describes the public path traffic takes.
fn is_tunnel_name(name: &str) -> bool {
    const TUNNEL_PREFIXES: [&str; 9] = ["utun", "tun", "tap", "wg", "ipsec", "ppp", "gif", "stf", "awdl"];
    TUNNEL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn is_routable_local(ip: IpAddr) -> bool {
    let is_special = ip.is_loopback() || ip.is_multicast() || ip.is_unspecified();
    let is_link_scoped = match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4.is_broadcast(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    };
    !is_special && !is_link_scoped
}

/// Connecting a UDP socket sends nothing; it only asks the routing table for the egress address.
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

/// Dropping it stops and joins the thread, so no callback outlives the session.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// A tunnel capture ends on its own when the device goes away.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
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

/// Returns once packets can arrive; a failure leaves any running capture untouched.
pub fn start(candidate: Candidate, session: Arc<SessionShared>) -> Result<Capture, String> {
    let (started_tx, started_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let host = Host {
        addresses: candidate.descriptor.addresses.iter().copied().collect(),
        mac: candidate.link.mac.map(|mac| mac.octets()),
        networks: candidate
            .link
            .ips
            .iter()
            .map(|network| (network.network(), network.prefix()))
            .collect(),
        side: candidate.side,
    };
    let thread = thread::Builder::new()
        .name("traffic-capture".to_string())
        .spawn(move || run(candidate.link, host, session, thread_stop, started_tx))
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
    host: Host,
    session: Arc<SessionShared>,
    stop: Arc<AtomicBool>,
    started: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let name = link.name.clone();
    let config = Config {
        read_timeout: Some(READ_TIMEOUT),
        read_buffer_size: READ_BUFFER_SIZE,
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

    let mut aggregates = Aggregates::new(host.side);
    let mut last_flush = Instant::now();
    let mut last_batch_ms = 0;
    loop {
        if stop.load(Ordering::Relaxed) || !session.is_active() {
            break;
        }
        match receiver.next() {
            Ok(frame) => {
                if let Some(observation) = classify::observe(frame, &host) {
                    let second = session.elapsed_ms() / 1000;
                    aggregates.record(second, observation);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                // A tunnel going away is the VPN disconnecting, not a broken session.
                if host.side == Side::Wire {
                    session.end(format!("capture on {name} stopped: {e}"));
                }
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

    #[test]
    fn only_ip_tunnel_devices_count_as_vpn_tunnels() {
        for name in ["utun8", "tun0", "wg0"] {
            assert!(is_vpn_tunnel_name(name), "{name}");
        }
        for name in ["en0", "tap0", "ipsec0", "ppp0", "awdl0", "gif0", "lo0"] {
            assert!(!is_vpn_tunnel_name(name), "{name}");
        }
    }

    #[test]
    fn the_vpn_tunnel_is_the_one_holding_the_egress_address() {
        let egress: IpAddr = "10.2.0.2".parse().expect("ip");
        let tunnel_addresses = [egress];
        assert!(carries_egress("utun8", &tunnel_addresses, egress));
        // Another tunnel device without that address.
        assert!(!carries_egress("utun2", &[], egress));
        // The public interface holding the egress address means no VPN is active.
        assert!(!carries_egress(
            "en0",
            &["192.168.1.20".parse().expect("ip")],
            "192.168.1.20".parse().expect("ip")
        ));
    }

    /// The tunnel capture sees the real destination of a packet sent into the VPN.
    #[test]
    #[ignore = "opens a live capture; needs capture privileges and a connected VPN"]
    fn live_tunnel_capture_sees_destinations() {
        use super::super::{HoprPeers, SessionShared};
        use gnosis_vpn_lib::traffic::Interface;

        let candidate = default_tunnel().expect("a VPN tunnel owning the default route");
        let session = Arc::new(SessionShared::new(
            1,
            Interface {
                id: "en0".to_string(),
                name: "en0".to_string(),
                addresses: Vec::new(),
                is_default: false,
            },
            HoprPeers::default(),
        ));
        let capture = start(candidate, session.clone()).expect("capture starts");

        let socket = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
        let _ = socket.send_to(&[0u8; 64], "1.1.1.1:9");
        std::thread::sleep(Duration::from_millis(500));
        let response = session.read(0);
        drop(capture);

        let destination: IpAddr = "1.1.1.1".parse().expect("ip");
        let deltas: Vec<_> = response.batches.iter().flat_map(|batch| batch.deltas.clone()).collect();
        assert!(
            deltas.iter().any(|delta| delta.target == destination && delta.tunnel),
            "expected 1.1.1.1 as a tunnel destination in {deltas:?}"
        );
    }
}
