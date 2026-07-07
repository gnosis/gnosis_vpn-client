//! Synchronous single-peer WireGuard state machine built on NepTUN's `Tunn`.
//!
//! `WgTunnel` owns one `Tunn` and turns each event (an outbound IP packet, an
//! inbound WireGuard datagram, a timer tick) into owned byte buffers to write to
//! the network or the TUN device. Keeping this core synchronous and allocation-
//! explicit makes the whole WireGuard protocol surface - handshake, data,
//! post-handshake drain, rekey/expiry, allowed-IPs enforcement - exhaustively
//! unit-testable against a second `WgTunnel` acting as the peer, with no async
//! plumbing involved.

use std::net::IpAddr;

use ipnetwork::IpNetwork;
use neptun::noise::errors::WireGuardError;
use neptun::noise::{Tunn, TunnResult};
use x25519_dalek::{PublicKey, StaticSecret};

use super::Error;
use crate::wireguard;

/// WireGuard's per-datagram overhead over the plaintext: a 16-byte data-message
/// header plus the 16-byte Poly1305 tag.
const WG_DATA_OVERHEAD: usize = 32;

/// Scratch buffer for a single NepTUN output. It must strictly dominate any
/// packet the pump can hand to `encapsulate`: the pump reads up to `MAX_FRAME`
/// bytes per packet, and encapsulation adds up to `WG_DATA_OVERHEAD`, so
/// `MAX_FRAME + WG_DATA_OVERHEAD` guarantees a full read always fits. Handshake
/// (148 B) and cookie (64 B) messages are far smaller.
const SCRATCH_LEN: usize = super::MAX_FRAME + WG_DATA_OVERHEAD;

/// Buffers to emit as the result of processing one inbound WireGuard datagram.
///
/// `to_network` holds WireGuard datagrams that must be written back to the
/// session (handshake responses and any packets flushed by the post-handshake
/// drain). `to_tun` holds decrypted IP packets destined for the local TUN device
/// whose source address passed the allowed-IPs check.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outputs {
    pub to_network: Vec<Vec<u8>>,
    pub to_tun: Vec<Vec<u8>>,
}

/// The result of a timer tick: WireGuard datagrams to send (handshake
/// retransmits, keepalives) plus whether the peer's session has expired and the
/// connection should be torn down and re-established.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TimerTick {
    pub to_network: Vec<Vec<u8>>,
    pub expired: bool,
}

/// The WireGuard data-plane operations the pump drives. Abstracting these behind
/// a trait lets the async pump be tested against a scripted engine while the real
/// `WgTunnel` is tested against a second `WgTunnel`.
pub trait TunnelEngine {
    /// Produce the handshake initiation datagram to bring the tunnel up eagerly.
    fn handshake_initiation(&mut self) -> Result<Vec<u8>, Error>;
    /// Encrypt one outbound IP packet, yielding the datagram to send (or `None`
    /// if the packet was queued pending an in-progress handshake).
    fn encapsulate(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>, Error>;
    /// Decrypt one inbound WireGuard datagram, running the post-handshake drain.
    fn decapsulate(&mut self, datagram: &[u8]) -> Result<Outputs, Error>;
    /// Advance WireGuard's timers, emitting any due datagrams.
    fn update_timers(&mut self) -> Result<TimerTick, Error>;
}

/// One decapsulate step, decoupled from the scratch buffer's borrow so the drain
/// loop can reuse the buffer across iterations.
enum Step {
    Done,
    Network(Vec<u8>),
    Tunnel(Vec<u8>, IpAddr),
}

pub struct WgTunnel {
    tunn: Tunn,
    scratch: Box<[u8]>,
    allowed_ips: Vec<IpNetwork>,
}

impl WgTunnel {
    /// Build a single-peer tunnel from base64 WireGuard keys (as stored in the
    /// gvpn config and returned by the gvpn registration). `allowed_ips` gates
    /// which source addresses decrypted (INGRESS) packets may carry; an empty
    /// list denies everything, matching WireGuard's cryptokey-routing semantics.
    /// Egress packets are not destination-filtered here - outbound scoping is
    /// delegated to the OS routing table (single peer, `Table = off`).
    pub fn new(
        private_key_b64: &str,
        peer_public_key_b64: &str,
        preshared_key_b64: Option<&str>,
        allowed_ips: &[IpNetwork],
    ) -> Result<Self, Error> {
        let secret = StaticSecret::from(wireguard::decode_key32(private_key_b64)?);
        let peer = PublicKey::from(wireguard::decode_key32(peer_public_key_b64)?);
        let preshared_key = match preshared_key_b64 {
            Some(psk) => Some(wireguard::decode_key32(psk)?),
            None => None,
        };
        // index 0, no persistent keepalive, no rate limiter - single-peer client,
        // matching the wg-quick config we are replacing.
        let tunn = Tunn::new(secret, peer, preshared_key, None, 0, None).map_err(Error::Tunn)?;
        Ok(Self {
            tunn,
            scratch: vec![0u8; SCRATCH_LEN].into_boxed_slice(),
            allowed_ips: allowed_ips.to_vec(),
        })
    }

    fn is_allowed(&self, src: IpAddr) -> bool {
        self.allowed_ips.iter().any(|net| net.contains(src))
    }

    fn decap_step(&mut self, src: Option<IpAddr>, datagram: &[u8]) -> Step {
        // Direct field access keeps `tunn` and `scratch` as disjoint borrows.
        match self.tunn.decapsulate(src, datagram, &mut self.scratch) {
            TunnResult::Done => Step::Done,
            TunnResult::WriteToNetwork(buf) => Step::Network(buf.to_vec()),
            TunnResult::WriteToTunnel(buf, src) => Step::Tunnel(buf.to_vec(), src),
            TunnResult::Err(e) => {
                // A decapsulation error is a per-datagram drop, never a teardown.
                // Over a reordering/duplicating mixnet, DuplicateCounter (replay
                // window) and NoCurrentSession (post-rekey stragglers) are
                // routine, and malformed/undecryptable datagrams must not kill
                // the tunnel. This mirrors NepTUN's own device driver
                // (log-and-continue); a persistently failing peer still
                // self-heals via the handshake-expiry path in `update_timers`.
                tracing::debug!(?e, "dropping inbound datagram: wireguard decapsulate error");
                Step::Done
            }
        }
    }
}

impl TunnelEngine for WgTunnel {
    fn handshake_initiation(&mut self) -> Result<Vec<u8>, Error> {
        match self.tunn.format_handshake_initiation(&mut self.scratch, false) {
            TunnResult::WriteToNetwork(buf) => Ok(buf.to_vec()),
            TunnResult::Done => Err(Error::Unexpected("handshake initiation produced no datagram")),
            TunnResult::WriteToTunnel(..) => Err(Error::Unexpected("handshake initiation wrote to tunnel")),
            TunnResult::Err(e) => Err(Error::WireGuard(e)),
        }
    }

    fn encapsulate(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        match self.tunn.encapsulate(packet, &mut self.scratch) {
            TunnResult::Done => Ok(None),
            TunnResult::WriteToNetwork(buf) => Ok(Some(buf.to_vec())),
            TunnResult::WriteToTunnel(..) => Err(Error::Unexpected("encapsulate wrote to tunnel")),
            TunnResult::Err(e) => {
                // A per-packet encapsulation failure (e.g. an oversized packet)
                // drops just that packet, matching NepTUN's own driver - never a
                // teardown.
                tracing::debug!(?e, "dropping outbound packet: wireguard encapsulate error");
                Ok(None)
            }
        }
    }

    fn decapsulate(&mut self, datagram: &[u8]) -> Result<Outputs, Error> {
        let mut outputs = Outputs::default();
        let mut step = self.decap_step(None, datagram);
        loop {
            match step {
                Step::Done => break,
                Step::Network(datagram) => {
                    // Post-handshake drain: NepTUN documents that after a
                    // WriteToNetwork the caller must keep decapsulating with an
                    // empty datagram until Done, flushing packets queued during
                    // the handshake.
                    outputs.to_network.push(datagram);
                    step = self.decap_step(None, &[]);
                }
                Step::Tunnel(packet, src) => {
                    if self.is_allowed(src) {
                        outputs.to_tun.push(packet);
                    } else {
                        tracing::debug!(%src, "dropping inbound packet: source not in allowed_ips");
                    }
                    break;
                }
            }
        }
        Ok(outputs)
    }

    fn update_timers(&mut self) -> Result<TimerTick, Error> {
        match self.tunn.update_timers(&mut self.scratch) {
            TunnResult::Done => Ok(TimerTick::default()),
            TunnResult::WriteToNetwork(buf) => Ok(TimerTick {
                to_network: vec![buf.to_vec()],
                expired: false,
            }),
            TunnResult::Err(WireGuardError::ConnectionExpired) => Ok(TimerTick {
                to_network: Vec::new(),
                expired: true,
            }),
            TunnResult::Err(e) => Err(Error::WireGuard(e)),
            TunnResult::WriteToTunnel(..) => Err(Error::Unexpected("update_timers wrote to tunnel")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::{BASE64_STANDARD, Engine as _};

    /// A client/server WgTunnel pair keyed to each other, with the server
    /// accepting the given allowed-IPs range from the client.
    fn tunnel_pair(server_allowed: &[IpNetwork]) -> (WgTunnel, WgTunnel) {
        let client_secret = StaticSecret::random();
        let server_secret = StaticSecret::random();
        let client_pub = BASE64_STANDARD.encode(PublicKey::from(&client_secret).to_bytes());
        let server_pub = BASE64_STANDARD.encode(PublicKey::from(&server_secret).to_bytes());
        let client_priv = BASE64_STANDARD.encode(client_secret.to_bytes());
        let server_priv = BASE64_STANDARD.encode(server_secret.to_bytes());

        let all_v4 = ["0.0.0.0/0".parse().unwrap()];
        let client = WgTunnel::new(&client_priv, &server_pub, None, &all_v4).expect("client");
        let server = WgTunnel::new(&server_priv, &client_pub, None, server_allowed).expect("server");
        (client, server)
    }

    /// Minimal well-formed 20-byte IPv4 header (what a real TUN delivers).
    fn ipv4_packet(src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
        let mut p = vec![0x45, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x40, 0x01, 0x00, 0x00];
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p
    }

    /// Minimal well-formed 40-byte IPv6 header: version 6, zero payload length,
    /// next-header 59 (no-next-header), src and dst in bytes 8..24 and 24..40.
    fn ipv6_packet(src: [u8; 16], dst: [u8; 16]) -> Vec<u8> {
        let mut p = vec![0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 59, 0x40];
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p
    }

    /// Drive the handshake to completion: client initiates, server responds,
    /// client consumes the response.
    fn complete_handshake(client: &mut WgTunnel, server: &mut WgTunnel) {
        let init = client.handshake_initiation().expect("init");
        let server_out = server.decapsulate(&init).expect("server handshake");
        assert_eq!(server_out.to_network.len(), 1, "server responds with one datagram");
        assert!(server_out.to_tun.is_empty());
        let client_out = client.decapsulate(&server_out.to_network[0]).expect("client handshake");
        assert!(client_out.to_tun.is_empty());
    }

    #[test]
    fn new_rejects_invalid_key() {
        let all = ["0.0.0.0/0".parse().unwrap()];
        let good = BASE64_STANDARD.encode([7u8; 32]);
        assert!(WgTunnel::new("not base64 !!", &good, None, &all).is_err());
        assert!(WgTunnel::new(&good, &BASE64_STANDARD.encode([0u8; 16]), None, &all).is_err());
    }

    #[test]
    fn handshake_then_bidirectional_data_roundtrips() {
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);

        // client -> server
        let up = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        let ciphertext = client.encapsulate(&up).expect("encap").expect("datagram");
        let server_out = server.decapsulate(&ciphertext).expect("decap");
        assert_eq!(server_out.to_tun, vec![up.clone()]);
        assert!(server_out.to_network.is_empty());

        // server -> client (server may send once it has received client data)
        let down = ipv4_packet([10, 128, 0, 1], [10, 0, 0, 2]);
        let ciphertext = server.encapsulate(&down).expect("encap").expect("datagram");
        let client_out = client.decapsulate(&ciphertext).expect("decap");
        assert_eq!(client_out.to_tun, vec![down]);
    }

    #[test]
    fn decapsulate_drains_packets_queued_during_handshake() {
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);

        // Encapsulating before any handshake initiates one AND queues the packet;
        // the queued packet is only released by the post-handshake drain.
        let queued = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        let init = client.encapsulate(&queued).expect("encap").expect("handshake datagram");

        let server_out = server.decapsulate(&init).expect("server handshake");
        assert_eq!(server_out.to_network.len(), 1);

        // Processing the handshake response completes the handshake, which flushes
        // both a keepalive and the packet queued during the handshake.
        let client_out = client.decapsulate(&server_out.to_network[0]).expect("client drain");
        assert!(
            !client_out.to_network.is_empty(),
            "handshake completion flushes queued traffic"
        );

        // Every flushed datagram is delivered to the server; the queued packet
        // must arrive decrypted among them (keepalives decrypt to nothing).
        let mut delivered = Vec::new();
        for datagram in &client_out.to_network {
            let out = server.decapsulate(datagram).expect("server decap flushed");
            delivered.extend(out.to_tun);
        }
        assert!(
            delivered.contains(&queued),
            "queued packet flushed and delivered by drain"
        );
    }

    #[test]
    fn decapsulate_rejects_source_outside_allowed_ips() {
        // Server only accepts 10.0.0.0/24 from the peer.
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);

        // In-range source is delivered.
        let allowed = ipv4_packet([10, 0, 0, 9], [10, 128, 0, 1]);
        let ct = client.encapsulate(&allowed).expect("encap").expect("dg");
        assert_eq!(server.decapsulate(&ct).expect("decap").to_tun, vec![allowed]);

        // Out-of-range source is dropped (empty to_tun), not delivered.
        let foreign = ipv4_packet([192, 168, 1, 5], [10, 128, 0, 1]);
        let ct = client.encapsulate(&foreign).expect("encap").expect("dg");
        let out = server.decapsulate(&ct).expect("decap");
        assert!(out.to_tun.is_empty(), "foreign-source packet must be dropped");
    }

    #[test]
    fn decapsulate_enforces_allowed_ips_for_ipv6_sources() {
        // The ingress source filter is family-agnostic: an IPv6 allowed-IPs range
        // admits an in-range IPv6 source and drops an out-of-range one, exactly as
        // for IPv4.
        let (mut client, mut server) = tunnel_pair(&["fd00::/8".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);

        let allowed = ipv6_packet(
            [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
            [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        );
        let ct = client.encapsulate(&allowed).expect("encap").expect("dg");
        assert_eq!(server.decapsulate(&ct).expect("decap").to_tun, vec![allowed]);

        let foreign = ipv6_packet(
            [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
            [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        );
        let ct = client.encapsulate(&foreign).expect("encap").expect("dg");
        let out = server.decapsulate(&ct).expect("decap");
        assert!(out.to_tun.is_empty(), "out-of-range IPv6 source must be dropped");
    }

    #[test]
    fn empty_allowed_ips_denies_all() {
        let (mut client, mut server) = tunnel_pair(&[]);
        complete_handshake(&mut client, &mut server);
        let pkt = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        let ct = client.encapsulate(&pkt).expect("encap").expect("dg");
        assert!(server.decapsulate(&ct).expect("decap").to_tun.is_empty());
    }

    #[test]
    fn decapsulate_of_garbage_is_dropped_not_fatal() {
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);
        // Random bytes cannot be decrypted; the datagram must be dropped, not
        // surfaced as a tunnel-killing error.
        let out = server
            .decapsulate(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33])
            .expect("no teardown");
        assert!(out.to_tun.is_empty() && out.to_network.is_empty());
    }

    #[test]
    fn decapsulate_of_replayed_datagram_is_dropped_not_fatal() {
        // Replays are routine over a reordering/duplicating mixnet; a duplicate
        // must be dropped (anti-replay) without tearing the tunnel down.
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);

        let pkt = ipv4_packet([10, 0, 0, 2], [10, 128, 0, 1]);
        let ct = client.encapsulate(&pkt).expect("encap").expect("dg");

        // First delivery succeeds.
        assert_eq!(server.decapsulate(&ct).expect("decap").to_tun, vec![pkt]);
        // Replaying the exact same ciphertext is silently dropped, still Ok.
        let replay = server.decapsulate(&ct).expect("replay must not tear down");
        assert!(replay.to_tun.is_empty() && replay.to_network.is_empty());
    }

    #[test]
    fn encapsulate_oversized_packet_is_dropped_not_fatal() {
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);
        // A packet larger than the scratch buffer can hold cannot be encrypted;
        // it is dropped (Ok(None)) rather than returning a fatal error.
        let oversized = vec![0x45u8; SCRATCH_LEN + 100];
        assert!(client.encapsulate(&oversized).expect("no teardown").is_none());
    }

    #[test]
    fn update_timers_on_fresh_tunnel_is_not_expired() {
        // Exercises the real update_timers mapping on the live engine: a freshly
        // handshaked tunnel is not expired and does not panic. (Rekey/expiry
        // TIMING is owned by NepTUN's own timer tests and the Phase 4 e2e
        // reconnect-storm test; the pump's reaction to `expired` is covered by
        // the scripted-engine test in the pump module.)
        let (mut client, mut server) = tunnel_pair(&["10.0.0.0/24".parse().unwrap()]);
        complete_handshake(&mut client, &mut server);
        let tick = client.update_timers().expect("timers ok");
        assert!(!tick.expired);
    }
}
