//! The async pump: a single task that carries packets between the local TUN
//! device and a `HoprSession` byte stream, driving a [`TunnelEngine`] in the
//! middle.
//!
//! It is deliberately a single `tokio::select!` loop over four endpoint halves
//! rather than a task-per-direction with a shared, locked `Tunn`: the loop owns
//! the engine outright (no mutex), and every `WriteToNetwork` becomes exactly one
//! `send` on the session, upholding WireGuard's one-datagram-per-frame contract.
//! Writes are awaited inline, so a slow session applies real backpressure to the
//! TUN reader - replacing the old UDP bridge's silent-drop ingress queue.

use std::time::Duration;

use super::Error;
use super::tunnel::TunnelEngine;

/// Cadence of NepTUN's timer processing, matching its own device loop.
const TIMER_PERIOD: Duration = Duration::from_millis(250);

/// Longest a single session/TUN write may block before the pump treats the
/// endpoint as wedged and tears down so the connection can be re-established.
/// Legitimate backpressure resolves in milliseconds; this only bounds a hang so
/// that expiry detection and reconnect can never be gated forever on a stuck
/// write.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Writes whole WireGuard datagrams to the session. Each call MUST become a
/// single session write so datagram boundaries survive as message frames.
#[async_trait::async_trait]
pub trait NetworkSender: Send {
    async fn send(&mut self, datagram: &[u8]) -> std::io::Result<()>;
}

/// Reads whole WireGuard datagrams from the session, one per call, or `None` on
/// clean close. Implementations must be cancel-safe (a single read, since this is
/// polled inside `select!`) and MUST NOT report a length greater than `buf.len()`.
///
/// The "exactly one datagram per `recv`" property is an ASSUMPTION at this layer:
/// WireGuard packets are not self-delimiting, so it holds only if the underlying
/// transport preserves message boundaries. Verifying it for a real `HoprSession`
/// (whether its `AsyncRead` can coalesce two frames under load) is spec risk #1,
/// resolved by the session adapter in a later phase - falling back to reading at
/// the boundary-preserving `Stream<ApplicationDataIn>` layer if needed. The
/// in-memory channel double used in tests preserves boundaries by construction.
#[async_trait::async_trait]
pub trait NetworkReceiver: Send {
    async fn recv(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>>;
}

/// Writes whole IP packets to the local TUN device.
#[async_trait::async_trait]
pub trait TunSender: Send {
    async fn send(&mut self, packet: &[u8]) -> std::io::Result<()>;
}

/// Reads whole IP packets from the local TUN device, one per call, or `None` on
/// close. Must be cancel-safe and MUST NOT report a length greater than
/// `buf.len()`.
#[async_trait::async_trait]
pub trait TunReceiver: Send {
    async fn recv(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>>;
}

/// Why the pump loop returned.
#[derive(Debug, PartialEq, Eq)]
pub enum PumpExit {
    /// The peer's WireGuard session expired; the caller should reconnect.
    Expired,
    /// The session (network) side closed.
    NetworkClosed,
    /// The TUN side closed.
    TunClosed,
}

/// Outcome of a bounded write to an endpoint.
enum Sent {
    Ok,
    /// The endpoint closed cleanly (broken pipe / connection reset).
    Closed,
}

/// Classify a timeout-wrapped write: a clean close (broken pipe/reset) is a
/// normal teardown signal, a timeout or any other io error is fatal.
fn classify(result: Result<std::io::Result<()>, tokio::time::error::Elapsed>) -> Result<Sent, Error> {
    match result {
        Err(_elapsed) => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "endpoint write timed out",
        ))),
        Ok(Ok(())) => Ok(Sent::Ok),
        Ok(Err(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(Sent::Closed)
        }
        Ok(Err(e)) => Err(Error::Io(e)),
    }
}

async fn send_network<NS: NetworkSender>(net_tx: &mut NS, datagram: &[u8]) -> Result<Sent, Error> {
    classify(tokio::time::timeout(SEND_TIMEOUT, net_tx.send(datagram)).await)
}

async fn send_tun<TS: TunSender>(tun_tx: &mut TS, packet: &[u8]) -> Result<Sent, Error> {
    classify(tokio::time::timeout(SEND_TIMEOUT, tun_tx.send(packet)).await)
}

/// Run the pump until an endpoint closes, the session expires, or a fatal error
/// occurs. Sends a handshake initiation up front so the tunnel comes up without
/// waiting for the first application packet.
///
/// Network writes are awaited inline, so a slow session backpressures the TUN
/// reader (the intended replacement for the old silent-drop queue). This does
/// mean a long outbound write briefly delays inbound and timer servicing
/// (head-of-line), which is acceptable since the mixnet, not WG crypto, is the
/// bottleneck; the send timeout bound guarantees a wedged write can never
/// stall expiry detection and reconnect forever.
pub async fn run<E, NS, NR, TS, TR>(
    mut engine: E,
    mut net_tx: NS,
    mut net_rx: NR,
    mut tun_tx: TS,
    mut tun_rx: TR,
) -> Result<PumpExit, Error>
where
    E: TunnelEngine + Send + 'static,
    NS: NetworkSender + 'static,
    NR: NetworkReceiver + 'static,
    TS: TunSender + 'static,
    TR: TunReceiver + 'static,
{
    let initiation = engine.handshake_initiation()?;
    if let Sent::Closed = send_network(&mut net_tx, &initiation).await? {
        return Ok(PumpExit::NetworkClosed);
    }

    let mut tun_buf = vec![0u8; super::MAX_FRAME];
    let mut net_buf = vec![0u8; super::MAX_FRAME];
    // Delay the first tick a full period; the handshake was just sent above.
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + TIMER_PERIOD, TIMER_PERIOD);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Outbound: a plaintext IP packet from the local TUN device.
            read = tun_rx.recv(&mut tun_buf) => {
                match read? {
                    None => return Ok(PumpExit::TunClosed),
                    Some(n) => {
                        debug_assert!(n <= tun_buf.len(), "TunReceiver reported oversized read");
                        if let Some(datagram) = engine.encapsulate(&tun_buf[..n])?
                            && let Sent::Closed = send_network(&mut net_tx, &datagram).await?
                        {
                            return Ok(PumpExit::NetworkClosed);
                        }
                    }
                }
            }
            // Inbound: a WireGuard datagram from the session.
            read = net_rx.recv(&mut net_buf) => {
                match read? {
                    None => return Ok(PumpExit::NetworkClosed),
                    Some(n) => {
                        debug_assert!(n <= net_buf.len(), "NetworkReceiver reported oversized read");
                        let outputs = engine.decapsulate(&net_buf[..n])?;
                        for datagram in outputs.to_network {
                            if let Sent::Closed = send_network(&mut net_tx, &datagram).await? {
                                return Ok(PumpExit::NetworkClosed);
                            }
                        }
                        for packet in outputs.to_tun {
                            if let Sent::Closed = send_tun(&mut tun_tx, &packet).await? {
                                return Ok(PumpExit::TunClosed);
                            }
                        }
                    }
                }
            }
            // Timer: drive WireGuard's own handshake/keepalive/expiry timers.
            _ = timer.tick() => {
                let tick = engine.update_timers()?;
                // Surface expiry before any send so a stuck write can never gate
                // the reconnect signal.
                if tick.expired {
                    return Ok(PumpExit::Expired);
                }
                for datagram in tick.to_network {
                    if let Sent::Closed = send_network(&mut net_tx, &datagram).await? {
                        return Ok(PumpExit::NetworkClosed);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use base64::prelude::{BASE64_STANDARD, Engine as _};
    use tokio::sync::mpsc::{Receiver, Sender, channel};
    use x25519_dalek::{PublicKey, StaticSecret};

    use super::*;
    use crate::wg_tunnel::tunnel::{Outputs, TimerTick, WgTunnel};

    /// An mpsc-backed endpoint half. Because each `Vec<u8>` is one message, it
    /// preserves datagram/packet boundaries perfectly - the ideal stand-in for a
    /// boundary-preserving `HoprSession` frame stream or a TUN device.
    struct ChannelTx(Sender<Vec<u8>>);
    struct ChannelRx(Receiver<Vec<u8>>);

    fn closed() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "channel closed")
    }

    #[async_trait::async_trait]
    impl NetworkSender for ChannelTx {
        async fn send(&mut self, datagram: &[u8]) -> std::io::Result<()> {
            self.0.send(datagram.to_vec()).await.map_err(|_| closed())
        }
    }
    #[async_trait::async_trait]
    impl TunSender for ChannelTx {
        async fn send(&mut self, packet: &[u8]) -> std::io::Result<()> {
            self.0.send(packet.to_vec()).await.map_err(|_| closed())
        }
    }
    #[async_trait::async_trait]
    impl NetworkReceiver for ChannelRx {
        async fn recv(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
            match self.0.recv().await {
                Some(msg) => {
                    buf[..msg.len()].copy_from_slice(&msg);
                    Ok(Some(msg.len()))
                }
                None => Ok(None),
            }
        }
    }
    #[async_trait::async_trait]
    impl TunReceiver for ChannelRx {
        async fn recv(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
            match self.0.recv().await {
                Some(msg) => {
                    buf[..msg.len()].copy_from_slice(&msg);
                    Ok(Some(msg.len()))
                }
                None => Ok(None),
            }
        }
    }

    /// A sender that accepts a write but never completes it - a wedged endpoint.
    struct WedgedTx;

    #[async_trait::async_trait]
    impl NetworkSender for WedgedTx {
        async fn send(&mut self, _datagram: &[u8]) -> std::io::Result<()> {
            std::future::pending().await
        }
    }

    /// A sender whose writes fail with a fixed io error kind.
    struct FailingTx(std::io::ErrorKind);

    #[async_trait::async_trait]
    impl NetworkSender for FailingTx {
        async fn send(&mut self, _datagram: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::new(self.0, "endpoint failure"))
        }
    }

    /// A programmable engine to test the pump's control flow in isolation from
    /// real crypto and real time.
    #[derive(Default)]
    struct ScriptedEngine {
        init: Vec<u8>,
        decap: VecDeque<Outputs>,
        ticks: VecDeque<TimerTick>,
    }
    impl TunnelEngine for ScriptedEngine {
        fn handshake_initiation(&mut self) -> Result<Vec<u8>, Error> {
            Ok(self.init.clone())
        }
        fn encapsulate(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>, Error> {
            // Echo the plaintext straight back as the "datagram" so tests can
            // observe the outbound path without crypto.
            Ok(Some(packet.to_vec()))
        }
        fn decapsulate(&mut self, _datagram: &[u8]) -> Result<Outputs, Error> {
            Ok(self.decap.pop_front().unwrap_or_default())
        }
        fn update_timers(&mut self) -> Result<TimerTick, Error> {
            Ok(self.ticks.pop_front().unwrap_or_default())
        }
    }

    fn ipv4_packet(src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
        let mut p = vec![0x45, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x40, 0x01, 0x00, 0x00];
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p
    }

    #[tokio::test]
    async fn pump_sends_handshake_initiation_before_anything_else() {
        let engine = ScriptedEngine {
            init: vec![0xaa, 0xbb, 0xcc],
            ..Default::default()
        };
        let (net_tx, mut net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let handle = tokio::spawn(run(
            engine,
            ChannelTx(net_tx),
            ChannelRx(net_in),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in),
        ));
        assert_eq!(net_out.recv().await, Some(vec![0xaa, 0xbb, 0xcc]));
        handle.abort();
    }

    #[tokio::test]
    async fn pump_writes_decapsulated_packets_to_tun() {
        let packet = ipv4_packet([10, 128, 0, 1], [10, 0, 0, 2]);
        let engine = ScriptedEngine {
            init: vec![1],
            decap: VecDeque::from([Outputs {
                to_network: vec![],
                to_tun: vec![packet.clone()],
            }]),
            ..Default::default()
        };
        let (net_tx, _net_out) = channel(8);
        let (tun_out_tx, mut tun_out) = channel(8);
        let (net_in_tx, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let handle = tokio::spawn(run(
            engine,
            ChannelTx(net_tx),
            ChannelRx(net_in),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in),
        ));
        net_in_tx.send(vec![0x01, 0x02]).await.unwrap();
        assert_eq!(tun_out.recv().await, Some(packet));
        handle.abort();
    }

    #[tokio::test]
    async fn pump_encapsulates_tun_packets_to_network() {
        let engine = ScriptedEngine {
            init: vec![0xff],
            ..Default::default()
        };
        let (net_tx, mut net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (tun_in_tx, tun_in) = channel::<Vec<u8>>(8);

        let handle = tokio::spawn(run(
            engine,
            ChannelTx(net_tx),
            ChannelRx(net_in),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in),
        ));
        // First datagram out is the handshake init; then our echoed packet.
        assert_eq!(net_out.recv().await, Some(vec![0xff]));
        let packet = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        tun_in_tx.send(packet.clone()).await.unwrap();
        assert_eq!(net_out.recv().await, Some(packet));
        handle.abort();
    }

    #[tokio::test]
    async fn pump_survives_a_dropped_inbound_datagram() {
        // A datagram the engine drops (replay/garbage/undecryptable) yields empty
        // Outputs; the pump must keep running rather than tearing down.
        let engine = ScriptedEngine {
            init: vec![1],
            decap: VecDeque::from([Outputs::default()]),
            ..Default::default()
        };
        let (net_tx, mut net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (net_in_tx, net_in) = channel::<Vec<u8>>(8);
        let (tun_in_tx, tun_in) = channel::<Vec<u8>>(8);

        let handle = tokio::spawn(run(
            engine,
            ChannelTx(net_tx),
            ChannelRx(net_in),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in),
        ));
        assert_eq!(net_out.recv().await, Some(vec![1])); // handshake init
        net_in_tx.send(vec![0xde, 0xad]).await.unwrap(); // dropped datagram
        // Still alive: a subsequent outbound packet is encapsulated as usual.
        let packet = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        tun_in_tx.send(packet.clone()).await.unwrap();
        assert_eq!(net_out.recv().await, Some(packet));
        handle.abort();
    }

    #[tokio::test]
    async fn pump_exits_cleanly_when_network_write_side_closes() {
        // A broken-pipe on write surfaces as PumpExit::NetworkClosed, symmetric
        // with the read-side close, rather than a raw error.
        let engine = ScriptedEngine {
            init: vec![1],
            ..Default::default()
        };
        let (net_tx, net_out) = channel(8);
        drop(net_out); // the very first send (handshake init) will fail
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let exit = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                engine,
                ChannelTx(net_tx),
                ChannelRx(net_in),
                ChannelTx(tun_out_tx),
                ChannelRx(tun_in),
            ),
        )
        .await
        .expect("no timeout")
        .expect("pump ok");
        assert_eq!(exit, PumpExit::NetworkClosed);
    }

    #[tokio::test(start_paused = true)]
    async fn pump_fails_when_a_network_write_wedges_past_the_send_timeout() {
        // A hung endpoint must surface as a fatal TimedOut error bounded by
        // SEND_TIMEOUT rather than stalling the pump (and expiry detection)
        // forever. Paused time auto-advances past the timeout while the
        // handshake-init write is wedged.
        let engine = ScriptedEngine {
            init: vec![1],
            ..Default::default()
        };
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let err = run(
            engine,
            WedgedTx,
            ChannelRx(net_in),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in),
        )
        .await
        .expect_err("a wedged write must be fatal");
        match err {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
            other => panic!("expected an io timeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pump_fails_on_a_non_close_network_write_error() {
        // Only BrokenPipe/ConnectionReset count as a clean endpoint close; any
        // other write error is fatal and must surface as Error::Io instead of a
        // PumpExit.
        let engine = ScriptedEngine {
            init: vec![1],
            ..Default::default()
        };
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let err = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                engine,
                FailingTx(std::io::ErrorKind::PermissionDenied),
                ChannelRx(net_in),
                ChannelTx(tun_out_tx),
                ChannelRx(tun_in),
            ),
        )
        .await
        .expect("no timeout")
        .expect_err("a non-close write error must be fatal");
        match err {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
            other => panic!("expected a fatal io error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pump_returns_expired_when_timers_report_expiry() {
        let engine = ScriptedEngine {
            init: vec![1],
            ticks: VecDeque::from([TimerTick {
                to_network: vec![],
                expired: true,
            }]),
            ..Default::default()
        };
        let (net_tx, _net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);

        let exit = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                engine,
                ChannelTx(net_tx),
                ChannelRx(net_in),
                ChannelTx(tun_out_tx),
                ChannelRx(tun_in),
            ),
        )
        .await
        .expect("pump should exit before timeout")
        .expect("pump ok");
        assert_eq!(exit, PumpExit::Expired);
    }

    #[tokio::test]
    async fn pump_exits_when_network_closes() {
        let engine = ScriptedEngine {
            init: vec![1],
            ..Default::default()
        };
        let (net_tx, _net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (net_in_tx, net_in) = channel::<Vec<u8>>(8);
        let (_keep_tun_in, tun_in) = channel::<Vec<u8>>(8);
        drop(net_in_tx); // close the network receive side

        let exit = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                engine,
                ChannelTx(net_tx),
                ChannelRx(net_in),
                ChannelTx(tun_out_tx),
                ChannelRx(tun_in),
            ),
        )
        .await
        .expect("no timeout")
        .expect("pump ok");
        assert_eq!(exit, PumpExit::NetworkClosed);
    }

    #[tokio::test]
    async fn pump_exits_when_tun_closes() {
        let engine = ScriptedEngine {
            init: vec![1],
            ..Default::default()
        };
        let (net_tx, _net_out) = channel(8);
        let (tun_out_tx, _tun_out) = channel(8);
        let (_keep_net_in, net_in) = channel::<Vec<u8>>(8);
        let (tun_in_tx, tun_in) = channel::<Vec<u8>>(8);
        drop(tun_in_tx); // close the TUN receive side

        let exit = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                engine,
                ChannelTx(net_tx),
                ChannelRx(net_in),
                ChannelTx(tun_out_tx),
                ChannelRx(tun_in),
            ),
        )
        .await
        .expect("no timeout")
        .expect("pump ok");
        assert_eq!(exit, PumpExit::TunClosed);
    }

    /// End-to-end through the real `WgTunnel` engine and a second `WgTunnel`
    /// acting as the exit-node WireGuard server, wired over boundary-preserving
    /// in-memory channels. Exercises handshake, encapsulate, session transport,
    /// decapsulate, drain and both directions of data flow through the pump.
    #[tokio::test]
    async fn pump_carries_data_both_ways_against_a_real_peer() {
        let client_secret = StaticSecret::random();
        let server_secret = StaticSecret::random();
        let client_pub = BASE64_STANDARD.encode(PublicKey::from(&client_secret).to_bytes());
        let server_pub = BASE64_STANDARD.encode(PublicKey::from(&server_secret).to_bytes());
        let client_priv = BASE64_STANDARD.encode(client_secret.to_bytes());
        let server_priv = BASE64_STANDARD.encode(server_secret.to_bytes());
        let all_v4 = ["0.0.0.0/0".parse().unwrap()];

        let client = WgTunnel::new(&client_priv, &server_pub, None, &all_v4).unwrap();
        let mut server = WgTunnel::new(&server_priv, &client_pub, None, &all_v4).unwrap();

        // client pump  --to_server-->  server driver
        //             <--to_client---
        let (to_server_tx, mut to_server_rx) = channel::<Vec<u8>>(16);
        let (to_client_tx, to_client_rx) = channel::<Vec<u8>>(16);
        let (tun_in_tx, tun_in_rx) = channel::<Vec<u8>>(16);
        let (tun_out_tx, mut tun_out_rx) = channel::<Vec<u8>>(16);
        let (captured_tx, mut captured_rx) = channel::<Vec<u8>>(16);

        let down_packet = ipv4_packet([10, 128, 0, 1], [10, 0, 0, 2]);
        let down_for_driver = down_packet.clone();

        // The server driver: decrypt whatever arrives, echo handshake/drain
        // datagrams back, record delivered packets, and reply once with a
        // down-path packet so the pump's inbound-to-TUN path runs on real crypto.
        let driver = tokio::spawn(async move {
            let mut replied = false;
            while let Some(datagram) = to_server_rx.recv().await {
                let out = server.decapsulate(&datagram).expect("server decap");
                for dg in out.to_network {
                    let _ = to_client_tx.send(dg).await;
                }
                for packet in out.to_tun {
                    let _ = captured_tx.send(packet).await;
                    if !replied {
                        replied = true;
                        if let Some(dg) = server.encapsulate(&down_for_driver).expect("server encap") {
                            let _ = to_client_tx.send(dg).await;
                        }
                    }
                }
            }
        });

        let pump = tokio::spawn(run(
            client,
            ChannelTx(to_server_tx),
            ChannelRx(to_client_rx),
            ChannelTx(tun_out_tx),
            ChannelRx(tun_in_rx),
        ));

        // Inject an outbound packet from the "applications" side of the TUN.
        let up_packet = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        tun_in_tx.send(up_packet.clone()).await.unwrap();

        // The server must receive it decrypted...
        let delivered = tokio::time::timeout(Duration::from_secs(5), captured_rx.recv())
            .await
            .expect("up packet delivered before timeout");
        assert_eq!(delivered, Some(up_packet));

        // ...and the server's reply must surface on the client's TUN.
        let received = tokio::time::timeout(Duration::from_secs(5), tun_out_rx.recv())
            .await
            .expect("down packet delivered before timeout");
        assert_eq!(received, Some(down_packet));

        pump.abort();
        driver.abort();
    }
}
