//! Pump network-endpoint adapters over a connected UDP socket.
//!
//! The NepTUN pump replaces the kernel WireGuard client that used to dial the
//! loopback UDP port exposed by the HOPR session bridge (`create_udp_client_binding`).
//! Instead of splicing the `HoprSession` directly, the pump's network side is a UDP
//! socket connected to that same `bound_host`: every `WriteToNetwork` becomes one
//! `send`, and each inbound datagram is one `recv`. UDP preserves message
//! boundaries, so "one datagram per recv" holds without any framing - the concern
//! flagged as spec risk #1 for a raw byte-stream splice does not arise here.

use std::io;
use std::sync::Arc;

use tokio::net::UdpSocket;

use super::{NetworkReceiver, NetworkSender};

/// Split a connected [`UdpSocket`] into pump network halves that share it. Both
/// `send` and `recv` take `&self`, so a single socket backs both directions.
pub fn udp_endpoints(socket: UdpSocket) -> (UdpSender, UdpReceiver) {
    let shared = Arc::new(socket);
    (UdpSender(shared.clone()), UdpReceiver(shared))
}

/// Writes whole WireGuard datagrams to the connected UDP socket.
pub struct UdpSender(Arc<UdpSocket>);

#[async_trait::async_trait]
impl NetworkSender for UdpSender {
    async fn send(&mut self, datagram: &[u8]) -> io::Result<()> {
        // Connected UDP: one send is exactly one datagram and is never short.
        self.0.send(datagram).await.map(|_| ())
    }
}

/// Reads whole WireGuard datagrams from the connected UDP socket, one per `recv`.
pub struct UdpReceiver(Arc<UdpSocket>);

#[async_trait::async_trait]
impl NetworkReceiver for UdpReceiver {
    async fn recv(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        // UDP has no EOF; a recv yields exactly one datagram. The bridge going
        // away surfaces as a recv/send error (teardown), never as a clean None.
        let n = self.0.recv(buf).await?;
        Ok(Some(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connected_pair() -> (UdpSocket, UdpSocket) {
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        a.connect(ba).await.unwrap();
        b.connect(aa).await.unwrap();
        (a, b)
    }

    #[tokio::test]
    async fn datagram_roundtrips_over_connected_udp() {
        let (a, b) = connected_pair().await;
        let (mut tx, _rx_a) = udp_endpoints(a);
        let (_tx_b, mut rx) = udp_endpoints(b);

        let datagram = vec![0x01, 0x02, 0x03, 0xff, 0xee];
        tx.send(&datagram).await.unwrap();

        let mut buf = vec![0u8; 2048];
        let n = rx.recv(&mut buf).await.unwrap().expect("a datagram");
        assert_eq!(&buf[..n], &datagram[..]);
    }

    #[tokio::test]
    async fn back_to_back_datagrams_keep_their_boundaries() {
        let (a, b) = connected_pair().await;
        let (mut tx, _rx_a) = udp_endpoints(a);
        let (_tx_b, mut rx) = udp_endpoints(b);

        let first = vec![1u8; 100];
        let second = vec![2u8; 1400];
        tx.send(&first).await.unwrap();
        tx.send(&second).await.unwrap();

        let mut buf = vec![0u8; 2048];
        let n1 = rx.recv(&mut buf).await.unwrap().expect("first");
        assert_eq!(&buf[..n1], &first[..]);
        let n2 = rx.recv(&mut buf).await.unwrap().expect("second");
        assert_eq!(&buf[..n2], &second[..]);
    }
}
