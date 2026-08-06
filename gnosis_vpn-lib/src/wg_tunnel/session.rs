//! Pump endpoint adapters over a byte-duplex session (the spliced `HoprSession`).
//!
//! The pump speaks in whole WireGuard datagrams: every [`NetworkSender::send`] is
//! one datagram, and every [`NetworkReceiver::recv`] must yield exactly one. A
//! `HoprSession` is an `AsyncRead + AsyncWrite` byte duplex, so these adapters map
//! "one datagram" onto "one write" and "one read". Splitting the session with
//! [`tokio::io::split`] hands the write half to [`SessionSender`] and the read
//! half to [`SessionReceiver`], which the pump then polls independently inside its
//! `select!`.
//!
//! # Frame boundaries
//!
//! WireGuard data messages are not self-delimiting, so `recv` returning "one
//! datagram" requires the transport to preserve message boundaries. It does, by
//! construction of the HOPR session this splice runs over:
//!
//! - The WG session is opened with `Capability::Segmentation`, so `HoprSession`'s
//!   read side is `into_async_read` over the reassembled-*frame* stream. That
//!   adapter yields the bytes of at most one frame per `read` — it never merges
//!   two frames into a single read.
//! - The session frame MTU is `max(configured, SESSION_MTU)`, and `SESSION_MTU`
//!   (~1458 B) exceeds a maximum WG data datagram: a 1420-MTU inner packet plus
//!   WireGuard's 32-byte data-message overhead is 1452 B. So a data datagram maps
//!   1:1 onto one frame — never split across reads (our buffer is `MAX_FRAME`,
//!   larger still) — and two full-size data datagrams cannot share one frame
//!   (2 × 1452 > 1458). Hence one `recv` returns exactly one WG data datagram.
//!   (Tiny control datagrams — a 32-byte keepalive, a handshake — could in
//!   principle share a frame; a coalesced trailing keepalive decrypts to nothing
//!   and a handshake is retransmitted, so neither corrupts data traffic.)
//!
//! This matches the production loopback-UDP bridge, which carried the same session
//! as a byte stream via `copy_duplex`. **Do not add length-prefix framing here:**
//! the exit node forwards the raw session payload to a stock WireGuard server over
//! UDP, so any length prefix we inject would be delivered as part of the ciphertext
//! and corrupt every packet. If a session is ever observed to desync wholesale, the
//! pump's decapsulation-failure guard tears it down and reconnects (see `pump`).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{NetworkReceiver, NetworkSender};

/// Writes whole WireGuard datagrams to the write half of a session. Each `send`
/// is one `write_all` + `flush`, so a datagram is never split across writes.
pub struct SessionSender<W> {
    write: W,
}

impl<W> SessionSender<W> {
    pub fn new(write: W) -> Self {
        Self { write }
    }
}

#[async_trait::async_trait]
impl<W> NetworkSender for SessionSender<W>
where
    W: AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, datagram: &[u8]) -> std::io::Result<()> {
        // One datagram per write upholds the pump's one-datagram-per-frame
        // contract; flush so a small datagram is not held in a buffer while the
        // peer waits for it.
        self.write.write_all(datagram).await?;
        self.write.flush().await
    }
}

/// Reads whole WireGuard datagrams from the read half of a session, one per
/// `recv`, or `None` on clean EOF.
pub struct SessionReceiver<R> {
    read: R,
}

impl<R> SessionReceiver<R> {
    pub fn new(read: R) -> Self {
        Self { read }
    }
}

#[async_trait::async_trait]
impl<R> NetworkReceiver for SessionReceiver<R>
where
    R: AsyncRead + Unpin + Send,
{
    async fn recv(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        // A single read is cancel-safe (required: this is polled in the pump's
        // `select!`). Under a boundary-preserving transport it returns exactly one
        // datagram; see the module-level frame-boundary note.
        let n = self.read.read(buf).await?;
        Ok(if n == 0 { None } else { Some(n) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single datagram written on one end of an in-memory duplex is received
    /// whole on the other, and lengths are preserved.
    #[tokio::test]
    async fn one_datagram_roundtrips_through_the_duplex() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let (s_r, _s_w) = tokio::io::split(server);

        let mut sender = SessionSender::new(c_w);
        let mut receiver = SessionReceiver::new(s_r);

        let datagram = vec![0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03];
        sender.send(&datagram).await.unwrap();

        let mut buf = vec![0u8; 2048];
        let n = receiver.recv(&mut buf).await.unwrap().expect("a datagram");
        assert_eq!(&buf[..n], &datagram[..]);
    }

    /// Closing the write side surfaces as a clean `None` (EOF) on `recv`, which
    /// the pump maps to `PumpExit::NetworkClosed` rather than an error.
    #[tokio::test]
    async fn recv_reports_none_on_clean_close() {
        let (client, server) = tokio::io::duplex(4096);
        let sender = SessionSender::new(client);
        let mut receiver = SessionReceiver::new(server);

        // Drop the whole client end so the peer read half actually sees EOF; a
        // `tokio::io::split` write half alone would keep the stream alive.
        drop(sender);
        assert_eq!(receiver.recv(&mut [0u8; 64]).await.unwrap(), None);
    }

    /// Back-to-back datagrams that are each read before the next is written keep
    /// their boundaries - the ordered, one-in-one-out path the pump relies on.
    #[tokio::test]
    async fn sequential_datagrams_preserve_boundaries() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let (s_r, _s_w) = tokio::io::split(server);

        let mut sender = SessionSender::new(c_w);
        let mut receiver = SessionReceiver::new(s_r);

        for payload in [vec![1u8; 10], vec![2u8; 1400], vec![3u8; 32]] {
            sender.send(&payload).await.unwrap();
            let mut buf = vec![0u8; 2048];
            let n = receiver.recv(&mut buf).await.unwrap().expect("datagram");
            assert_eq!(&buf[..n], &payload[..]);
        }
    }

    /// Two datagrams written back-to-back before a single `recv` are COALESCED into
    /// one read over a raw `tokio::io::duplex`: a bare byte pipe preserves no
    /// message boundaries. This is the WORST CASE, not the real transport - the
    /// adapter itself does not frame. On a real `HoprSession` the segmented
    /// frame-stream read layer plus the frame-MTU sizing keep one data datagram to
    /// one read (see the module-level "Frame boundaries" note); this test pins down
    /// what the adapter does NOT guarantee on its own, and the pump's
    /// decapsulation-failure guard is the backstop if a session ever desyncs.
    #[tokio::test]
    async fn back_to_back_writes_can_coalesce_into_one_read() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let (s_r, _s_w) = tokio::io::split(server);
        let mut sender = SessionSender::new(c_w);
        let mut receiver = SessionReceiver::new(s_r);

        // Both datagrams are written (and buffered) before any read is issued.
        sender.send(&[1u8; 8]).await.unwrap();
        sender.send(&[2u8; 8]).await.unwrap();

        let mut buf = vec![0u8; 2048];
        let n = receiver.recv(&mut buf).await.unwrap().expect("data");
        // The single read returns both datagrams concatenated, proving the boundary
        // is not preserved by the adapter.
        assert_eq!(n, 16);
        assert_eq!(&buf[..8], &[1u8; 8]);
        assert_eq!(&buf[8..16], &[2u8; 8]);
    }
}
