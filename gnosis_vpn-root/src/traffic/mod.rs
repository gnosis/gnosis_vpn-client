//! Opt-in capture of the traffic that leaves the host on its public interface.
//!
//! Only one session is current. Starting a capture is transactional: the
//! candidate opens first and a failure leaves the running session and its
//! history untouched. The session keeps a short bounded window of finished
//! batches; retention and presentation belong to the consuming application.
//!
//! Capture stops on its own when the consumer stops reading. A viewer that dies
//! must not leave a privileged capture running.

pub mod capture;
pub mod classify;

use std::collections::{HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use gnosis_vpn_lib::command::{Command, Response};
use gnosis_vpn_lib::traffic::{
    self, Batch, Class, Interface, PROTOCOL_VERSION, ReadRequest, ReadResponse, StartRequest, StartResponse,
    StopRequest, StopResponse,
};
use tokio::task::JoinHandle;

/// Group whose members may drive capture; the worker's group, per the installer.
const CLIENT_GROUP: &str = gnosis_vpn_lib::worker::GROUPNAME;
/// Finished batches kept for a consumer that has not read them yet. At the
/// daemon's flush cadence this covers roughly two seconds.
const BATCH_CAPACITY: usize = 20;
/// A consumer that stops reading ends the session, so a crashed viewer cannot
/// leave a capture running.
const LEASE: Duration = Duration::from_millis(5000);
const MONITOR_INTERVAL: Duration = Duration::from_millis(500);

/// Addresses announced by the HOPR node, used only to associate captured traffic
/// with the mixnet transport.
#[derive(Debug, Default)]
pub struct PeerAddresses {
    addresses: RwLock<HashSet<IpAddr>>,
}

impl PeerAddresses {
    pub fn replace(&self, addresses: Vec<IpAddr>) {
        let mut current = self.addresses.write().expect("peer addresses poisoned");
        current.clear();
        current.extend(addresses);
    }

    pub fn clear(&self) {
        self.addresses.write().expect("peer addresses poisoned").clear();
    }

    pub fn classify(&self, target: IpAddr) -> Class {
        if self
            .addresses
            .read()
            .expect("peer addresses poisoned")
            .contains(&target)
        {
            Class::HoprAssociated
        } else {
            Class::Other
        }
    }
}

#[derive(Default)]
struct BatchLog {
    batches: VecDeque<Batch>,
    next_sequence: u64,
    dropped: u64,
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
}

impl SessionShared {
    fn new(id: u64, interface: Interface, stop: Arc<AtomicBool>) -> Self {
        Self {
            id,
            interface,
            start: Instant::now(),
            stop,
            batches: Mutex::new(BatchLog::default()),
            ended: Mutex::new(None),
            finished: AtomicBool::new(false),
            last_read_ms: AtomicU64::new(0),
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

    pub fn push(&self, deltas: Vec<traffic::Delta>, elapsed_ms: u64) {
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
}

impl ActiveSession {
    fn shutdown(mut self) {
        self.monitor.abort();
        self.shared.finish();
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
    }
}

/// Whether a socket peer may control traffic capture.
///
/// Capture is a privileged monitoring capability, so it is limited to root, the
/// worker user, and members of the client's group. Desktop users are granted that
/// group at install time, which is what lets the app drive capture without
/// opening the API to every local account.
pub fn peer_may_capture(uid: u32, gid: u32, worker_uid: u32) -> bool {
    if uid == 0 || uid == worker_uid {
        return true;
    }
    let in_client_group = |group: &uzers::Group| group.name().to_string_lossy() == CLIENT_GROUP;
    if uzers::get_group_by_gid(gid).is_some_and(|group| in_client_group(&group)) {
        return true;
    }
    let Some(user) = uzers::get_user_by_uid(uid) else {
        return false;
    };
    uzers::get_user_groups(user.name(), user.primary_group_id())
        .is_some_and(|groups| groups.iter().any(in_client_group))
}

/// Whether `cmd` belongs to the traffic capture API.
pub fn is_traffic_command(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::TrafficInterfaces | Command::TrafficStart(_) | Command::TrafficRead(_) | Command::TrafficStop(_)
    )
}

/// Owns the current capture session.
pub struct TrafficService {
    peers: Arc<PeerAddresses>,
    current: Option<ActiveSession>,
    last_session_id: u64,
}

impl Default for TrafficService {
    fn default() -> Self {
        Self::new()
    }
}

impl TrafficService {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(PeerAddresses::default()),
            current: None,
            last_session_id: 0,
        }
    }

    /// Replaces the announced-peer set. Classification is applied at ingestion,
    /// so history keeps the class it was captured with.
    pub fn update_peer_addresses(&self, addresses: Vec<IpAddr>) {
        self.peers.replace(addresses);
    }

    pub fn clear_peer_addresses(&self) {
        self.peers.clear();
    }

    /// Answers traffic commands; returns `None` for anything else.
    pub async fn handle(&mut self, cmd: Command) -> Option<Response> {
        match cmd {
            Command::TrafficInterfaces => Some(Response::TrafficInterfaces(self.interfaces())),
            Command::TrafficStart(request) => Some(Response::TrafficStart(self.start(request).await)),
            Command::TrafficRead(request) => Some(Response::TrafficRead(self.read(request))),
            Command::TrafficStop(request) => Some(Response::TrafficStop(self.stop(request))),
            _ => None,
        }
    }

    fn interfaces(&self) -> Vec<Interface> {
        capture::discover()
            .into_iter()
            .map(|candidate| candidate.descriptor)
            .collect()
    }

    async fn start(&mut self, request: StartRequest) -> Result<StartResponse, String> {
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
        let shared = Arc::new(SessionShared::new(id, interface.clone(), stop.clone()));
        // Open the candidate before disturbing the running session. Opening blocks
        // until the capture thread reports back, so keep it off the command loop.
        let capture = {
            let session = shared.clone();
            let peers = self.peers.clone();
            run_blocking(move || capture::start(candidate, session, peers, stop))
                .await
                .map_err(|e| format!("Cannot start capture task: {e}"))??
        };

        if let Some(previous) = self.current.take() {
            previous.shutdown();
        }
        self.last_session_id = id;
        let monitor = spawn_monitor(shared.clone());
        self.current = Some(ActiveSession {
            shared,
            capture: Some(capture),
            monitor,
        });
        tracing::info!(target: "traffic", session = id, interface = %interface.id, "traffic session started");
        Ok(StartResponse {
            session_id: id,
            interface,
            protocol: PROTOCOL_VERSION,
        })
    }

    fn read(&mut self, request: ReadRequest) -> ReadResponse {
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

    fn stop(&mut self, request: StopRequest) -> StopResponse {
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

#[cfg(test)]
mod tests {
    use super::*;
    use gnosis_vpn_lib::traffic::Direction;

    fn session() -> Arc<SessionShared> {
        Arc::new(SessionShared::new(
            1,
            Interface {
                id: "en0".to_string(),
                name: "en0".to_string(),
                addresses: vec!["192.0.2.2".parse().expect("ip")],
                is_default: true,
            },
            Arc::new(AtomicBool::new(false)),
        ))
    }

    fn delta(second: u64, target: &str) -> traffic::Delta {
        traffic::Delta {
            second,
            target: target.parse().expect("ip"),
            class: Class::Other,
            direction: Direction::Upstream,
            packets: 1,
            bytes: 60,
            largest_packet: 60,
        }
    }

    #[test]
    fn read_returns_only_unseen_batches_and_advances_the_cursor() {
        let shared = session();
        shared.push(vec![delta(0, "203.0.113.1")], 100);
        shared.push(vec![delta(1, "203.0.113.1")], 1100);

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
            shared.push(vec![delta(sequence, "203.0.113.1")], sequence * 100);
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
        let session = Arc::new(SessionShared::new(1, interface.clone(), stop.clone()));
        let capture = capture::start(candidate, session.clone(), Arc::new(PeerAddresses::default()), stop)
            .expect("capture starts");

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
    }

    /// Drives the command surface the socket dispatcher calls: list, start, read
    /// and stop, including a second start replacing the first session.
    #[tokio::test]
    #[ignore = "opens a live capture; needs capture privileges"]
    async fn service_answers_the_traffic_commands() {
        use std::net::UdpSocket;
        use std::time::Duration;

        let mut service = TrafficService::new();
        let interfaces = match service.handle(Command::TrafficInterfaces).await {
            Some(Response::TrafficInterfaces(interfaces)) => interfaces,
            other => panic!("unexpected interfaces response: {other:?}"),
        };
        let interface = interfaces
            .iter()
            .find(|interface| interface.is_default)
            .expect("an egress interface")
            .clone();

        let started = match service
            .handle(Command::TrafficStart(StartRequest {
                interface_id: interface.id.clone(),
            }))
            .await
        {
            Some(Response::TrafficStart(Ok(started))) => started,
            other => panic!("unexpected start response: {other:?}"),
        };
        assert_eq!(started.protocol, PROTOCOL_VERSION);

        let socket = UdpSocket::bind("0.0.0.0:0").expect("udp socket");
        let _ = socket.send_to(&[0u8; 64], "1.1.1.1:53");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let read = match service
            .handle(Command::TrafficRead(ReadRequest {
                session_id: started.session_id,
                cursor: 0,
            }))
            .await
        {
            Some(Response::TrafficRead(read)) => read,
            other => panic!("unexpected read response: {other:?}"),
        };
        assert!(read.active);
        assert!(!read.gap);
        assert!(read.cursor > 0);

        // Starting the same interface again is a no-op, keeping the session.
        let again = match service
            .handle(Command::TrafficStart(StartRequest {
                interface_id: interface.id.clone(),
            }))
            .await
        {
            Some(Response::TrafficStart(Ok(again))) => again,
            other => panic!("unexpected restart response: {other:?}"),
        };
        assert_eq!(again.session_id, started.session_id);

        let stopped = match service
            .handle(Command::TrafficStop(StopRequest {
                session_id: started.session_id,
            }))
            .await
        {
            Some(Response::TrafficStop(stopped)) => stopped,
            other => panic!("unexpected stop response: {other:?}"),
        };
        assert!(stopped.stopped);
        service.shutdown();
    }

    #[test]
    fn capture_control_is_limited_to_root_the_worker_and_the_client_group() {
        assert!(peer_may_capture(0, 0, 501));
        assert!(peer_may_capture(501, 20, 501));
        // A uid without an account is never authorized.
        assert!(!peer_may_capture(4_294_967_294, 4_294_967_294, 501));
    }

    #[test]
    fn peer_addresses_associate_only_announced_targets() {
        let peers = PeerAddresses::default();
        let peer: IpAddr = "203.0.113.9".parse().expect("ip");
        let other: IpAddr = "198.51.100.4".parse().expect("ip");
        assert_eq!(peers.classify(peer), Class::Other);
        peers.replace(vec![peer]);
        assert_eq!(peers.classify(peer), Class::HoprAssociated);
        assert_eq!(peers.classify(other), Class::Other);
        peers.clear();
        assert_eq!(peers.classify(peer), Class::Other);
    }
}
