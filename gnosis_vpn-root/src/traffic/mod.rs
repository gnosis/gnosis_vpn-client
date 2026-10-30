//! Opt-in capture of the traffic that leaves the host on its public interface.
//!
//! Only one session is current. Starting a capture is transactional: the
//! candidate opens first and a failure leaves the running session and its
//! history untouched. The session keeps a short bounded window of finished
//! batches; retention and presentation belong to the consuming application.
//!
//! Capture stops on its own when the consumer stops reading. A viewer that dies
//! must not leave a privileged capture running.
//!
//! While a session runs, it also follows the VPN tunnel that owns the default
//! route and captures there, so the consumer sees destinations as well as the
//! next hops they travel through.

pub mod capture;
pub mod classify;

use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use gnosis_vpn_lib::traffic::{
    self, Batch, Interface, PROTOCOL_VERSION, ReadRequest, ReadResponse, StartRequest, StartResponse, StopRequest,
    StopResponse,
};
use tokio::task::JoinHandle;

/// About two seconds of batches from the wire and tunnel threads, which push into one log.
const BATCH_CAPACITY: usize = 40;
/// A consumer that stops reading ends the session, so a crashed viewer cannot
/// leave a capture running.
const LEASE: Duration = Duration::from_millis(5000);
const MONITOR_INTERVAL: Duration = Duration::from_millis(500);
/// How quickly a session notices a VPN connecting, disconnecting or switching
/// devices.
const TUNNEL_CHECK_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct BatchLog {
    batches: VecDeque<Batch>,
    next_sequence: u64,
    dropped: u64,
}

/// Addresses HOPR nodes announced on chain, as the worker last reported them.
/// Shared between the service and its sessions so a new snapshot applies to the
/// session that is already running.
#[derive(Clone, Default)]
pub struct HoprPeers(Arc<RwLock<HashSet<IpAddr>>>);

impl HoprPeers {
    pub fn replace(&self, ips: &[IpAddr]) {
        let mut peers = self.0.write().expect("hopr peers poisoned");
        *peers = ips.iter().copied().collect();
    }

    fn mark(&self, deltas: &mut [traffic::Delta]) {
        let peers = self.0.read().expect("hopr peers poisoned");
        for delta in deltas {
            // Inside a tunnel the target is a destination, not a hop.
            delta.hopr_peer = !delta.tunnel && peers.contains(&delta.target);
        }
    }
}

/// State shared by the capture thread, the lease monitor and the command loop.
pub struct SessionShared {
    pub id: u64,
    pub interface: Interface,
    start: Instant,
    stop: Arc<AtomicBool>,
    batches: Mutex<BatchLog>,
    ended: Mutex<Option<String>>,
    finished: AtomicBool,
    last_read_ms: AtomicU64,
    hopr_peers: HoprPeers,
}

impl SessionShared {
    fn new(id: u64, interface: Interface, stop: Arc<AtomicBool>, hopr_peers: HoprPeers) -> Self {
        Self {
            id,
            interface,
            start: Instant::now(),
            stop,
            batches: Mutex::new(BatchLog::default()),
            ended: Mutex::new(None),
            finished: AtomicBool::new(false),
            last_read_ms: AtomicU64::new(0),
            hopr_peers,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub fn is_active(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    fn idle_ms(&self) -> u64 {
        self.elapsed_ms()
            .saturating_sub(self.last_read_ms.load(Ordering::Relaxed))
    }

    fn mark_read(&self) {
        self.last_read_ms.store(self.elapsed_ms(), Ordering::Relaxed);
    }

    pub fn push(&self, mut deltas: Vec<traffic::Delta>, elapsed_ms: u64) {
        self.hopr_peers.mark(&mut deltas);
        let mut log = self.batches.lock().expect("batch log poisoned");
        // Make room first so the batch that causes a drop reports it.
        if log.batches.len() >= BATCH_CAPACITY {
            log.batches.pop_front();
            log.dropped += 1;
        }
        let batch = Batch {
            protocol: PROTOCOL_VERSION,
            session_id: self.id,
            sequence: log.next_sequence,
            elapsed_ms,
            deltas,
            dropped_batches: log.dropped,
        };
        log.next_sequence += 1;
        log.batches.push_back(batch);
    }

    /// Ends the session with a reason a consumer can surface once.
    pub fn end(&self, reason: String) {
        let mut ended = self.ended.lock().expect("session reason poisoned");
        if ended.is_none() {
            *ended = Some(reason);
        }
        self.finish();
    }

    fn finish(&self) {
        self.finished.store(true, Ordering::Relaxed);
        self.stop.store(true, Ordering::Relaxed);
    }

    fn read(&self, cursor: u64) -> ReadResponse {
        let log = self.batches.lock().expect("batch log poisoned");
        let oldest = log
            .batches
            .front()
            .map(|batch| batch.sequence)
            .unwrap_or(log.next_sequence);
        let batches: Vec<Batch> = log
            .batches
            .iter()
            .filter(|batch| batch.sequence >= cursor)
            .cloned()
            .collect();
        ReadResponse {
            session_id: self.id,
            batches,
            cursor: log.next_sequence,
            gap: cursor < oldest,
            active: self.is_active(),
            ended: self.ended.lock().expect("session reason poisoned").clone(),
        }
    }
}

struct ActiveSession {
    shared: Arc<SessionShared>,
    capture: Option<capture::Capture>,
    monitor: JoinHandle<()>,
    tunnel_follower: JoinHandle<()>,
}

impl ActiveSession {
    fn shutdown(mut self) {
        self.monitor.abort();
        // Aborting drops the tunnel capture, which stops and joins its thread.
        self.tunnel_follower.abort();
        self.shared.finish();
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
    }
}

/// Owns the current capture session.
pub struct TrafficService {
    current: Option<ActiveSession>,
    last_session_id: u64,
    hopr_peers: HoprPeers,
}

impl Default for TrafficService {
    fn default() -> Self {
        Self::new()
    }
}

impl TrafficService {
    pub fn new() -> Self {
        Self {
            current: None,
            last_session_id: 0,
            hopr_peers: HoprPeers::default(),
        }
    }

    /// Takes a new snapshot of announced HOPR peer addresses. Applies to the
    /// running session as well as later ones.
    pub fn update_hopr_peers(&self, peer_ips: &[Ipv4Addr]) {
        let ips: Vec<IpAddr> = peer_ips.iter().copied().map(IpAddr::V4).collect();
        self.hopr_peers.replace(&ips);
    }

    pub fn interfaces(&self) -> Vec<Interface> {
        capture::discover()
            .into_iter()
            .map(|candidate| candidate.descriptor)
            .collect()
    }

    pub async fn start(&mut self, request: StartRequest) -> Result<StartResponse, String> {
        if let Some(current) = &self.current
            && current.shared.interface.id == request.interface_id
            && current.shared.is_active()
        {
            return Ok(StartResponse {
                session_id: current.shared.id,
                interface: current.shared.interface.clone(),
                protocol: PROTOCOL_VERSION,
            });
        }

        let candidate = capture::discover()
            .into_iter()
            .find(|candidate| candidate.descriptor.id == request.interface_id)
            .ok_or_else(|| format!("Interface {} is not available", request.interface_id))?;
        let interface = candidate.descriptor.clone();
        let id = self.last_session_id + 1;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(SessionShared::new(
            id,
            interface.clone(),
            stop.clone(),
            self.hopr_peers.clone(),
        ));
        // Open the candidate before disturbing the running session. Opening blocks
        // until the capture thread reports back, so keep it off the command loop.
        let capture = {
            let session = shared.clone();
            run_blocking(move || capture::start(candidate, session, stop))
                .await
                .map_err(|e| format!("Cannot start capture task: {e}"))??
        };

        if let Some(previous) = self.current.take() {
            previous.shutdown();
        }
        self.last_session_id = id;
        let monitor = spawn_monitor(shared.clone());
        let tunnel_follower = spawn_tunnel_follower(shared.clone());
        self.current = Some(ActiveSession {
            shared,
            capture: Some(capture),
            monitor,
            tunnel_follower,
        });
        tracing::info!(target: "traffic", session = id, interface = %interface.id, "traffic session started");
        Ok(StartResponse {
            session_id: id,
            interface,
            protocol: PROTOCOL_VERSION,
        })
    }

    pub fn read(&mut self, request: ReadRequest) -> ReadResponse {
        let Some(current) = &self.current else {
            return ReadResponse {
                session_id: request.session_id,
                batches: Vec::new(),
                cursor: request.cursor,
                gap: false,
                active: false,
                ended: Some("no capture session".to_string()),
            };
        };
        if current.shared.id != request.session_id {
            return ReadResponse {
                session_id: request.session_id,
                batches: Vec::new(),
                cursor: request.cursor,
                gap: true,
                active: false,
                ended: Some("capture session was replaced".to_string()),
            };
        }
        current.shared.mark_read();
        current.shared.read(request.cursor)
    }

    pub fn stop(&mut self, request: StopRequest) -> StopResponse {
        let matches = self
            .current
            .as_ref()
            .is_some_and(|current| current.shared.id == request.session_id);
        if !matches {
            return StopResponse { stopped: false };
        }
        if let Some(current) = self.current.take() {
            current.shutdown();
            tracing::info!(target: "traffic", session = request.session_id, "traffic session stopped");
        }
        StopResponse { stopped: true }
    }

    /// Stops capture. Safe to call when nothing is running.
    pub fn shutdown(&mut self) {
        if let Some(current) = self.current.take() {
            current.shutdown();
        }
    }
}

impl Drop for TrafficService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Runs a blocking capture operation on the blocking pool.
async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(operation).await
}

fn spawn_monitor(session: Arc<SessionShared>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(MONITOR_INTERVAL).await;
            if !session.is_active() {
                break;
            }
            if session.idle_ms() > LEASE.as_millis() as u64 {
                tracing::info!(target: "traffic", session = session.id, "capture lease expired");
                session.end("capture lease expired".to_string());
                break;
            }
        }
    })
}

/// Keeps a second capture on the VPN tunnel that owns the default route, and
/// moves it when the VPN connects, disconnects or switches devices.
fn spawn_tunnel_follower(session: Arc<SessionShared>) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Keyed by the whole descriptor: a reconnect can keep the device name but change its address.
        let mut current: Option<(Interface, capture::Capture)> = None;
        // A device that refused capture is not retried until the tunnel changes.
        let mut refused: Option<Interface> = None;
        while session.is_active() {
            let wanted = run_blocking(capture::default_tunnel).await.ok().flatten();
            let wanted_tunnel = wanted.as_ref().map(|candidate| candidate.descriptor.clone());
            let following = current
                .as_ref()
                .is_some_and(|(tunnel, capture)| Some(tunnel) == wanted_tunnel.as_ref() && capture.is_running());
            if !following {
                if let Some((tunnel, capture)) = current.take() {
                    let _ = run_blocking(move || capture.stop()).await;
                    tracing::info!(target: "traffic", session = session.id, tunnel = %tunnel.id, "tunnel capture stopped");
                }
                if refused != wanted_tunnel {
                    refused = None;
                }
                if let Some(candidate) = wanted.filter(|_| refused.is_none()) {
                    let tunnel = candidate.descriptor.clone();
                    let shared = session.clone();
                    let stop = Arc::new(AtomicBool::new(false));
                    match run_blocking(move || capture::start(candidate, shared, stop)).await {
                        Ok(Ok(capture)) => {
                            tracing::info!(target: "traffic", session = session.id, tunnel = %tunnel.id, "tunnel capture started");
                            current = Some((tunnel, capture));
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(target: "traffic", session = session.id, tunnel = %tunnel.id, error = %e, "cannot capture on tunnel");
                            refused = Some(tunnel);
                        }
                        Err(e) => {
                            tracing::warn!(target: "traffic", session = session.id, tunnel = %tunnel.id, error = %e, "cannot start tunnel capture task");
                            refused = Some(tunnel);
                        }
                    }
                }
            }
            tokio::time::sleep(TUNNEL_CHECK_INTERVAL).await;
        }
        if let Some((_, capture)) = current {
            let _ = run_blocking(move || capture.stop()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnosis_vpn_lib::traffic::Direction;

    fn session() -> Arc<SessionShared> {
        session_with_peers(HoprPeers::default())
    }

    fn session_with_peers(hopr_peers: HoprPeers) -> Arc<SessionShared> {
        Arc::new(SessionShared::new(
            1,
            Interface {
                id: "en0".to_string(),
                name: "en0".to_string(),
                addresses: vec!["192.0.2.2".parse().expect("ip")],
                is_default: true,
            },
            Arc::new(AtomicBool::new(false)),
            hopr_peers,
        ))
    }

    fn delta(second: u64) -> traffic::Delta {
        delta_to(second, "203.0.113.1")
    }

    fn delta_to(second: u64, target: &str) -> traffic::Delta {
        traffic::Delta {
            second,
            target: target.parse().expect("ip"),
            direction: Direction::Upstream,
            bytes: 60,
            hopr_peer: false,
            tunnel: false,
        }
    }

    fn marked_targets(response: &ReadResponse) -> Vec<(String, bool)> {
        let mut targets: Vec<(String, bool)> = response
            .batches
            .iter()
            .flat_map(|batch| batch.deltas.iter())
            .map(|delta| (delta.target.to_string(), delta.hopr_peer))
            .collect();
        targets.sort();
        targets
    }

    #[test]
    fn deltas_to_announced_hopr_peers_are_marked() {
        let peers = HoprPeers::default();
        peers.replace(&["203.0.113.1".parse().expect("ip")]);
        let shared = session_with_peers(peers);
        shared.push(vec![delta_to(0, "203.0.113.1"), delta_to(0, "198.51.100.4")], 100);

        assert_eq!(
            marked_targets(&shared.read(0)),
            vec![("198.51.100.4".to_string(), false), ("203.0.113.1".to_string(), true)]
        );
    }

    #[test]
    fn destinations_inside_a_tunnel_are_never_marked_as_hopr_peers() {
        let peers = HoprPeers::default();
        peers.replace(&["203.0.113.1".parse().expect("ip")]);
        let shared = session_with_peers(peers);
        let inside = traffic::Delta {
            tunnel: true,
            ..delta_to(0, "203.0.113.1")
        };
        shared.push(vec![inside], 100);

        assert_eq!(
            marked_targets(&shared.read(0)),
            vec![("203.0.113.1".to_string(), false)]
        );
    }

    #[test]
    fn a_peer_snapshot_replaces_the_previous_one_for_running_sessions() {
        let peers = HoprPeers::default();
        peers.replace(&["203.0.113.1".parse().expect("ip")]);
        let shared = session_with_peers(peers.clone());
        shared.push(vec![delta_to(0, "203.0.113.1")], 100);

        peers.replace(&["198.51.100.4".parse().expect("ip")]);
        shared.push(vec![delta_to(1, "203.0.113.1"), delta_to(1, "198.51.100.4")], 1100);

        let response = shared.read(1);
        assert_eq!(
            marked_targets(&response),
            vec![("198.51.100.4".to_string(), true), ("203.0.113.1".to_string(), false)]
        );
    }

    #[test]
    fn the_service_shares_reported_peers_with_its_sessions() {
        let service = TrafficService::new();
        service.update_hopr_peers(&["203.0.113.1".parse().expect("ip")]);
        let shared = session_with_peers(service.hopr_peers.clone());
        shared.push(vec![delta(0)], 100);

        assert_eq!(marked_targets(&shared.read(0)), vec![("203.0.113.1".to_string(), true)]);
    }

    #[test]
    fn read_returns_only_unseen_batches_and_advances_the_cursor() {
        let shared = session();
        shared.push(vec![delta(0)], 100);
        shared.push(vec![delta(1)], 1100);

        let first = shared.read(0);
        assert_eq!(first.batches.len(), 2);
        assert_eq!(first.cursor, 2);
        assert!(!first.gap);
        assert!(first.active);

        let second = shared.read(first.cursor);
        assert!(second.batches.is_empty());
        assert_eq!(second.cursor, 2);
        assert!(!second.gap);
    }

    #[test]
    fn batches_beyond_capacity_drop_oldest_and_report_a_gap() {
        let shared = session();
        for sequence in 0..(BATCH_CAPACITY as u64 + 5) {
            shared.push(vec![delta(sequence)], sequence * 100);
        }
        let response = shared.read(0);
        assert_eq!(response.batches.len(), BATCH_CAPACITY);
        assert_eq!(response.batches.first().map(|batch| batch.sequence), Some(5));
        assert!(response.gap);
        assert_eq!(response.cursor, BATCH_CAPACITY as u64 + 5);
        assert_eq!(response.batches.last().map(|batch| batch.dropped_batches), Some(5));
    }

    #[test]
    fn ending_a_session_reports_the_reason_once() {
        let shared = session();
        shared.end("capture on en0 stopped: broken pipe".to_string());
        shared.end("later reason".to_string());
        let response = shared.read(0);
        assert!(!response.active);
        assert_eq!(response.ended.as_deref(), Some("capture on en0 stopped: broken pipe"));
    }

    #[test]
    fn stopping_silently_leaves_no_reason() {
        let shared = session();
        shared.finish();
        let response = shared.read(0);
        assert!(!response.active);
        assert_eq!(response.ended, None);
    }

    /// Exercises the real capture path: discovery, socket capture, classification,
    /// aggregation and batch publication. Needs capture privileges and a network
    /// path, so it stays out of the default run.
    #[test]
    #[ignore = "opens a live capture; needs capture privileges and network traffic"]
    fn live_capture_publishes_batches() {
        use std::net::UdpSocket;
        use std::time::Duration;

        let candidate = capture::discover()
            .into_iter()
            .find(|candidate| candidate.descriptor.is_default)
            .expect("an egress interface");
        let interface = candidate.descriptor.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let session = Arc::new(SessionShared::new(
            1,
            interface.clone(),
            stop.clone(),
            HoprPeers::default(),
        ));
        let capture = capture::start(candidate, session.clone(), stop).expect("capture starts");

        let socket = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
        for destination in ["1.1.1.1:53", "8.8.8.8:53", "9.9.9.9:53"] {
            let _ = socket.send_to(&[0u8; 64], destination);
        }
        std::thread::sleep(Duration::from_millis(1500));
        let response = session.read(0);
        capture.stop();

        assert!(
            !response.batches.is_empty(),
            "expected at least one batch from {interface:?}"
        );
        let deltas: Vec<traffic::Delta> = response.batches.iter().flat_map(|batch| batch.deltas.clone()).collect();
        assert!(
            deltas
                .iter()
                .any(|delta| delta.direction == gnosis_vpn_lib::traffic::Direction::Upstream),
            "expected outbound traffic in {deltas:?}"
        );
        assert!(
            deltas
                .iter()
                .any(|delta| delta.target == "1.1.1.1".parse::<std::net::IpAddr>().expect("ip")),
            "expected 1.1.1.1 as a target in {deltas:?}"
        );
    }

    /// Drives the command surface the socket dispatcher calls: list, start, read
    /// and stop, including a second start replacing the first session.
    #[tokio::test]
    #[ignore = "opens a live capture; needs capture privileges"]
    async fn service_answers_the_traffic_commands() {
        use std::net::UdpSocket;
        use std::time::Duration;

        let mut service = TrafficService::new();
        let interface = service
            .interfaces()
            .into_iter()
            .find(|interface| interface.is_default)
            .expect("an egress interface");

        let started = service
            .start(StartRequest {
                interface_id: interface.id.clone(),
            })
            .await
            .expect("start response");
        assert_eq!(started.protocol, PROTOCOL_VERSION);

        let socket = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
        let _ = socket.send_to(&[0u8; 64], "1.1.1.1:53");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let read = service.read(ReadRequest {
            session_id: started.session_id,
            cursor: 0,
        });
        assert!(read.active);
        assert!(!read.gap);
        assert!(read.cursor > 0);

        // Starting the same interface again is a no-op, keeping the session.
        let again = service
            .start(StartRequest {
                interface_id: interface.id.clone(),
            })
            .await
            .expect("restart response");
        assert_eq!(again.session_id, started.session_id);

        let stopped = service.stop(StopRequest {
            session_id: started.session_id,
        });
        assert!(stopped.stopped);
        service.shutdown();
    }

    /// Starts a session on a public interface while a VPN owns the default route
    /// and checks that the follower adds the tunnel: destinations show up as
    /// tunnel deltas next to the wire deltas of the same session.
    #[tokio::test]
    #[ignore = "opens a live capture; needs capture privileges and a connected VPN"]
    async fn service_follows_the_vpn_tunnel() {
        use std::net::UdpSocket;
        use std::time::Duration;

        let mut service = TrafficService::new();
        // While a VPN owns the default route no public interface is the default,
        // so name the one the VPN's own traffic leaves through.
        let wire_name = std::env::var("GVPN_WIRE_INTERFACE").unwrap_or_else(|_| "en0".to_string());
        let wire = capture::discover()
            .into_iter()
            .find(|candidate| candidate.descriptor.id == wire_name)
            .expect("the public interface")
            .descriptor;
        let started = service
            .start(StartRequest {
                interface_id: wire.id.clone(),
            })
            .await
            .expect("start response");
        // The follower checks for a tunnel as soon as the session starts.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let socket = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
        let _ = socket.send_to(&[0u8; 64], "1.1.1.1:9");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let read = service.read(ReadRequest {
            session_id: started.session_id,
            cursor: 0,
        });
        service.shutdown();

        let deltas: Vec<traffic::Delta> = read.batches.iter().flat_map(|batch| batch.deltas.clone()).collect();
        let destination: std::net::IpAddr = "1.1.1.1".parse().expect("ip");
        assert!(
            deltas.iter().any(|delta| delta.tunnel && delta.target == destination),
            "expected 1.1.1.1 inside the tunnel in {deltas:?}"
        );
        assert!(
            deltas.iter().any(|delta| !delta.tunnel),
            "expected wire traffic to the VPN server in {deltas:?}"
        );
    }
}
