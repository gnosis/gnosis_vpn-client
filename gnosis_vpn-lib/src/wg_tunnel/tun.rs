//! Pump endpoint adapters over a raw TUN file descriptor.
//!
//! Root creates the TUN device and passes its fd to the worker (see
//! [`crate::socket::fd_passing`]); the worker wraps that fd here and drives it
//! with NepTUN. A TUN device is packet-oriented - one `read` yields exactly one IP
//! packet, one `write` injects exactly one - so the adapters need no framing of
//! their own, unlike the byte-duplex session side.
//!
//! Both a [`TunReader`] and a [`TunWriter`] share one [`AsyncFd`] over the fd via
//! an `Arc`, so the pump can poll readable and writable readiness on the same
//! device independently inside its `select!`.
//!
//! # Platform header
//!
//! Linux TUN opened with `IFF_NO_PI` carries no per-packet header
//! ([`PLATFORM_TUN_HEADER_LEN`] `= 0`). macOS `utun` prefixes every packet with a
//! 4-byte address-family header in network byte order
//! ([`PLATFORM_TUN_HEADER_LEN`] `= 4`); [`TunReader`] strips it and [`TunWriter`]
//! prepends it, chosen from the packet's IP version.

#![deny(unsafe_code)]

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use tokio::io::unix::AsyncFd;

use super::{TunReceiver, TunSender};

/// Length of the platform's per-packet TUN header: 0 on Linux (`IFF_NO_PI`), 4 on
/// macOS (`utun` address-family prefix).
#[cfg(target_os = "macos")]
pub const PLATFORM_TUN_HEADER_LEN: usize = 4;
#[cfg(not(target_os = "macos"))]
pub const PLATFORM_TUN_HEADER_LEN: usize = 0;

/// BSD `AF_INET` (2) as a 4-byte big-endian `utun` header.
const AF_INET_BE: [u8; 4] = [0, 0, 0, 2];
/// BSD `AF_INET6` (30) as a 4-byte big-endian `utun` header.
const AF_INET6_BE: [u8; 4] = [0, 0, 0, 30];

/// The macOS `utun` header for an outbound packet, selected from its IP version
/// (the high nibble of the first byte). Unknown/empty packets default to IPv4.
fn utun_header(packet: &[u8]) -> [u8; 4] {
    match packet.first().map(|b| b >> 4) {
        Some(6) => AF_INET6_BE,
        _ => AF_INET_BE,
    }
}

fn read_fd(fd: impl AsFd, buf: &mut [u8]) -> io::Result<usize> {
    rustix::io::read(fd, buf).map_err(io::Error::from)
}

fn write_fd(fd: impl AsFd, buf: &[u8]) -> io::Result<usize> {
    rustix::io::write(fd, buf).map_err(io::Error::from)
}

fn require_complete_packet_write(written: usize, expected: usize) -> io::Result<()> {
    if written == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("short TUN write: wrote {written} of {expected} bytes"),
        ))
    }
}

/// Wrap a TUN fd into a reader/writer pair sharing one non-blocking [`AsyncFd`].
///
/// `header_len` is the per-packet platform header ([`PLATFORM_TUN_HEADER_LEN`]);
/// pass `0` for a headerless device or `4` for macOS `utun`. Must be called from
/// within a Tokio runtime (it registers the fd with the reactor).
pub fn tun_endpoints(fd: OwnedFd, header_len: usize) -> io::Result<(TunReader, TunWriter)> {
    rustix::io::ioctl_fionbio(&fd, true).map_err(io::Error::from)?;
    let shared = Arc::new(AsyncFd::new(fd)?);
    Ok((
        TunReader {
            inner: shared.clone(),
            header_len,
        },
        TunWriter {
            inner: shared,
            header_len,
        },
    ))
}

/// Reads whole IP packets from the TUN device, one per `recv`, stripping the
/// platform header.
pub struct TunReader {
    inner: Arc<AsyncFd<OwnedFd>>,
    header_len: usize,
}

#[async_trait::async_trait]
impl TunReceiver for TunReader {
    async fn recv(&mut self, out: &mut [u8]) -> io::Result<Option<usize>> {
        // Read into a header-inclusive scratch, then hand back just the packet.
        let mut scratch = [0u8; super::MAX_FRAME + 4];
        loop {
            let mut guard = self.inner.readable().await?;
            match guard.try_io(|fd| read_fd(fd, &mut scratch)) {
                Ok(Ok(0)) => return Ok(None),
                Ok(Ok(n)) => {
                    let start = self.header_len.min(n);
                    let packet = &scratch[start..n];
                    let m = packet.len().min(out.len());
                    out[..m].copy_from_slice(&packet[..m]);
                    return Ok(Some(m));
                }
                Ok(Err(e)) => return Err(e),
                // Spurious readiness: readiness was cleared, poll again.
                Err(_would_block) => continue,
            }
        }
    }
}

/// Writes whole IP packets to the TUN device, prepending the platform header.
pub struct TunWriter {
    inner: Arc<AsyncFd<OwnedFd>>,
    header_len: usize,
}

#[async_trait::async_trait]
impl TunSender for TunWriter {
    async fn send(&mut self, packet: &[u8]) -> io::Result<()> {
        if packet.len() > super::MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TUN packet exceeds maximum frame size",
            ));
        }
        let mut framed = [0u8; super::MAX_FRAME + 4];
        let payload: &[u8] = if self.header_len == 0 {
            packet
        } else {
            let header = utun_header(packet);
            framed[..header.len()].copy_from_slice(&header);
            framed[header.len()..header.len() + packet.len()].copy_from_slice(packet);
            &framed[..header.len() + packet.len()]
        };
        loop {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|fd| write_fd(fd, payload)) {
                // A short write on a packet device drops the tail. Never retry it
                // as a second packet; reject the incomplete write instead.
                Ok(Ok(n)) => return require_complete_packet_write(n, payload.len()),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected, message-preserving (`SOCK_DGRAM`) socket pair standing in for
    /// the packet-oriented TUN device: one `write` on the peer becomes exactly one
    /// `read` on the endpoint. `a` is the endpoint fd; `b` is the test's peer.
    fn dgram_pair() -> (OwnedFd, OwnedFd) {
        rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::DGRAM,
            rustix::net::SocketFlags::empty(),
            None,
        )
        .expect("socketpair failed")
    }

    fn ipv4_packet() -> Vec<u8> {
        let mut p = vec![0x45, 0x00, 0x00, 0x14, 0, 0, 0, 0, 0x40, 0x01, 0, 0];
        p.extend_from_slice(&[10, 0, 0, 2, 10, 128, 0, 1]);
        p
    }

    #[test]
    fn utun_header_selects_address_family_by_ip_version() {
        assert_eq!(utun_header(&ipv4_packet()), AF_INET_BE);
        assert_eq!(utun_header(&[0x60, 0x00, 0x00, 0x00]), AF_INET6_BE);
        assert_eq!(utun_header(&[]), AF_INET_BE, "empty defaults to IPv4");
    }

    #[tokio::test]
    async fn headerless_read_yields_the_raw_packet() {
        let (a, b) = dgram_pair();
        let (mut reader, _writer) = tun_endpoints(a, 0).unwrap();
        let packet = ipv4_packet();
        assert_eq!(write_fd(&b, &packet).unwrap(), packet.len());

        let mut out = vec![0u8; 2048];
        let n = reader.recv(&mut out).await.unwrap().expect("a packet");
        assert_eq!(&out[..n], &packet[..]);
    }

    #[tokio::test]
    async fn utun_read_strips_the_four_byte_header() {
        let (a, b) = dgram_pair();
        let (mut reader, _writer) = tun_endpoints(a, 4).unwrap();
        let packet = ipv4_packet();
        // Peer delivers a utun-framed packet: [AF header][packet].
        let mut framed = AF_INET_BE.to_vec();
        framed.extend_from_slice(&packet);
        write_fd(&b, &framed).unwrap();

        let mut out = vec![0u8; 2048];
        let n = reader.recv(&mut out).await.unwrap().expect("a packet");
        assert_eq!(&out[..n], &packet[..], "the 4-byte header must be stripped");
    }

    #[tokio::test]
    async fn utun_write_prepends_the_four_byte_header() {
        let (a, b) = dgram_pair();
        let (_reader, mut writer) = tun_endpoints(a, 4).unwrap();
        let packet = ipv4_packet();
        writer.send(&packet).await.unwrap();

        let mut buf = vec![0u8; 2048];
        let n = read_fd(&b, &mut buf).unwrap();
        assert_eq!(&buf[..4], &AF_INET_BE, "IPv4 utun header prepended");
        assert_eq!(&buf[4..n], &packet[..]);
    }

    #[tokio::test]
    async fn write_rejects_packet_larger_than_max_frame() {
        let (a, _b) = dgram_pair();
        let (_reader, mut writer) = tun_endpoints(a, 0).unwrap();
        let packet = vec![0x45; super::super::MAX_FRAME + 1];
        let err = writer.send(&packet).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(err.to_string(), "TUN packet exceeds maximum frame size");
    }

    #[test]
    fn short_packet_write_is_rejected() {
        let err = require_complete_packet_write(7, 20).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
        assert_eq!(err.to_string(), "short TUN write: wrote 7 of 20 bytes");
    }

    #[tokio::test]
    async fn reader_and_writer_share_one_fd_both_directions() {
        // The reader and writer built from the same fd must work concurrently
        // against the peer: writer -> peer, and peer -> reader.
        let (a, b) = dgram_pair();
        let (mut reader, mut writer) = tun_endpoints(a, 0).unwrap();

        let out_packet = ipv4_packet();
        writer.send(&out_packet).await.unwrap();
        let mut peer_buf = vec![0u8; 2048];
        let n = read_fd(&b, &mut peer_buf).unwrap();
        assert_eq!(&peer_buf[..n], &out_packet[..], "writer reached the peer");

        let in_packet = ipv4_packet();
        write_fd(&b, &in_packet).unwrap();
        let mut buf = vec![0u8; 2048];
        let m = reader.recv(&mut buf).await.unwrap().expect("a packet");
        assert_eq!(&buf[..m], &in_packet[..], "reader saw the peer's packet");
    }
}
