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

use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time;

use super::{NetworkReceiver, NetworkSender};

/// Bounds every close of a spliced session: a dead session must not stall teardown.
pub const SESSION_CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Owns a session's write half for its whole life and closes it exactly once: through `shutdown()` if the owner gets there, otherwise on drop.
///
/// Why a drop guard and not an explicit close on every path: `HoprSession` has no Drop impl, and core
/// cancels the connect runner with a token-biased `run_until_cancelled`, which drops the runner future
/// at whatever await it sits on. After the open and before the pump owns the session there is no code
/// location left to call a close from, only `Drop`. The two error paths could close explicitly, but
/// that would leave cancellation as a separately handled special case for the same resource.
///
/// Why the `Option`: `Drop` cannot await, so the bounded close is spawned, and a spawned task must own
/// the writer. Moving a field out of `&mut self` needs something left behind, and there is no dummy
/// `WriteHalf<HoprSession>`; the safe alternatives are this `Option` or an `unsafe` `ManuallyDrop::take`.
/// It is `Some` for the guard's whole observable life and taken only in `Drop`.
pub(crate) struct CloseOnDrop<T: AsyncWrite + Unpin + Send + 'static> {
    session: Option<T>,
    closed: bool,
}

impl<T> CloseOnDrop<T>
where
    T: AsyncWrite + Unpin + Send + 'static,
{
    pub(crate) fn new(session: T) -> Self {
        Self {
            session: Some(session),
            closed: false,
        }
    }

    fn inner(&mut self) -> Pin<&mut T> {
        Pin::new(self.session.as_mut().expect("session is taken only by Drop"))
    }
}

impl<T> AsyncWrite for CloseOnDrop<T>
where
    T: AsyncWrite + Unpin + Send + 'static,
{
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        self.inner().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = ready!(self.inner().poll_shutdown(cx));
        // A finished shutdown, failed or not, is not retried on drop.
        self.closed = true;
        Poll::Ready(result)
    }
}

impl<T> Drop for CloseOnDrop<T>
where
    T: AsyncWrite + Unpin + Send + 'static,
{
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let Some(mut session) = self.session.take() else {
            return;
        };
        // Drop cannot await; a spawned close covers both the error path and a cancelled connect.
        tokio::spawn(async move {
            match time::timeout(SESSION_CLOSE_BUDGET, session.shutdown()).await {
                Ok(Err(error)) => tracing::warn!(%error, "failed to close abandoned wg session"),
                Err(_) => tracing::warn!("closing abandoned wg session timed out"),
                Ok(Ok(())) => tracing::debug!("closed abandoned wg session"),
            }
        });
    }
}

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

    async fn close(&mut self) -> std::io::Result<()> {
        self.write.shutdown().await
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// The teardown path: EOF from `close` alone, since the pump task still owns the session.
    #[tokio::test]
    async fn close_reports_none_without_dropping_the_sender() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let mut sender = SessionSender::new(c_w);
        let mut receiver = SessionReceiver::new(server);

        sender.close().await.unwrap();

        assert_eq!(receiver.recv(&mut [0u8; 64]).await.unwrap(), None);
    }

    /// The abort path: a dropped guard closes the session on its own.
    #[tokio::test]
    async fn dropping_the_guard_closes_the_session() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let mut receiver = SessionReceiver::new(server);

        drop(CloseOnDrop::new(c_w));

        assert_eq!(receiver.recv(&mut [0u8; 64]).await.unwrap(), None);
    }

    /// The happy path: the guard is transparent to the pump's writes.
    #[tokio::test]
    async fn the_guard_passes_writes_through() {
        let (client, server) = tokio::io::duplex(4096);
        let (_c_r, c_w) = tokio::io::split(client);
        let mut receiver = SessionReceiver::new(server);

        let mut sender = SessionSender::new(CloseOnDrop::new(c_w));
        sender.send(&[7u8; 8]).await.unwrap();

        let mut buf = [0u8; 64];
        let n = receiver.recv(&mut buf).await.unwrap().expect("a datagram");
        assert_eq!(&buf[..n], &[7u8; 8]);
    }

    /// Counts shutdowns; a duplex cannot tell one close from two.
    #[derive(Clone, Default)]
    struct CountingWriter {
        shutdowns: Arc<AtomicUsize>,
    }

    impl AsyncWrite for CountingWriter {
        fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    /// Lets the close spawned by `Drop` run on the test runtime.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    /// The teardown path: the pump's explicit close is the one and only close.
    #[tokio::test]
    async fn an_explicit_close_is_not_repeated_on_drop() {
        let writer = CountingWriter::default();
        let mut sender = SessionSender::new(CloseOnDrop::new(writer.clone()));

        sender.close().await.unwrap();
        drop(sender);
        settle().await;

        assert_eq!(writer.shutdowns.load(Ordering::SeqCst), 1);
    }

    /// The abort path closes exactly once as well.
    #[tokio::test]
    async fn a_bare_drop_closes_exactly_once() {
        let writer = CountingWriter::default();

        drop(CloseOnDrop::new(writer.clone()));
        settle().await;

        assert_eq!(writer.shutdowns.load(Ordering::SeqCst), 1);
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
