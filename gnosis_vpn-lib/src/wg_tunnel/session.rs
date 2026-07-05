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
//! # Frame-boundary assumption (spec risk #1)
//!
//! WireGuard datagrams are not self-delimiting, so `recv` returning "one datagram"
//! holds only if the transport preserves message boundaries: one peer `send` must
//! surface as one local `recv` of the same length. The gvpn WG session is a
//! `SessionTarget::UdpStream`, whose datagram semantics are expected to preserve
//! boundaries, matching what the old loopback-UDP bridge relied on. If a real
//! session is observed to coalesce two datagrams into one read under load, switch
//! these adapters to explicit length-prefix framing (write a `u16` length before
//! each datagram in `send`; read the prefix then exactly that many bytes in
//! `recv`) or read at the boundary-preserving `Stream<ApplicationDataIn>` layer.
//! The framing change is local to this file and does not touch the pump.

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
}
