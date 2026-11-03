//! Wire contract for public-interface traffic capture; packets stay in the root daemon, only per-second byte counts leave it.

use serde::{Deserialize, Serialize};

use std::net::IpAddr;

/// Bumped on any incompatible change; consumers refuse batches with an unknown version.
pub const PROTOCOL_VERSION: u16 = 5;

/// One interface the daemon is willing to capture on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    /// Stable capture identifier, accepted back by [`Request::TrafficStart`](crate::Request::TrafficStart).
    pub id: String,
    /// Human-readable name for the interface picker.
    pub name: String,
    /// Usable local addresses; used by the daemon to decide packet direction.
    pub addresses: Vec<IpAddr>,
    /// Whether the host's egress route currently uses this interface.
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Host to target.
    Upstream,
    /// Target to host.
    Downstream,
}

/// Bytes for one endpoint, direction and second; consumers sum deltas that share those keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    /// Whole seconds since the capture session started.
    pub second: u64,
    /// The remote endpoint of the packets.
    pub target: IpAddr,
    pub direction: Direction,
    /// Sum of complete IP-layer lengths, including the IP header.
    pub bytes: u64,
    /// `target` was announced by a HOPR node; never set for tunnel destinations or without a running node.
    pub hopr_peer: bool,
    /// Seen inside the VPN tunnel, so `target` is the destination rather than the next hop.
    pub tunnel: bool,
}

/// A bounded slice of aggregated traffic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub protocol: u16,
    pub session_id: u64,
    /// Consecutive per session, starting at 0.
    pub sequence: u64,
    /// Advances on idle batches too, so consumers can age their view.
    pub elapsed_ms: u64,
    pub deltas: Vec<Delta>,
    /// Batches the daemon discarded because the consumer fell behind.
    pub dropped_batches: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub interface_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResponse {
    pub session_id: u64,
    pub interface: Interface,
    pub protocol: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StartReply {
    Started(StartResponse),
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRequest {
    pub session_id: u64,
    /// First sequence number the consumer has not seen yet.
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResponse {
    pub session_id: u64,
    pub batches: Vec<Batch>,
    /// Cursor to send with the next read.
    pub cursor: u64,
    /// The cursor fell out of the retained window; treat the gap as missing data, not zero.
    pub gap: bool,
    /// Whether capture is still running.
    pub active: bool,
    /// Set once when the capture session ended without an explicit stop.
    pub ended: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRequest {
    pub session_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopResponse {
    pub stopped: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_round_trips_through_json() {
        let batch = Batch {
            protocol: PROTOCOL_VERSION,
            session_id: 7,
            sequence: 3,
            elapsed_ms: 1200,
            deltas: vec![Delta {
                second: 1,
                target: "192.0.2.10".parse().expect("ip"),
                direction: Direction::Downstream,
                bytes: 5120,
                hopr_peer: true,
                tunnel: false,
            }],
            dropped_batches: 0,
        };
        let json = serde_json::to_string(&batch).expect("serialize");
        assert!(json.contains("\"direction\":\"downstream\""), "{json}");
        assert!(json.contains("\"bytes\":5120"), "{json}");
        assert!(json.contains("\"target\":\"192.0.2.10\""), "{json}");
        assert!(json.contains("\"hopr_peer\":true"), "{json}");
        assert!(json.contains("\"tunnel\":false"), "{json}");
        assert!(!json.contains("class"), "{json}");
        let parsed: Batch = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, batch);
    }

    #[test]
    fn read_response_carries_cursor_and_gap() {
        let response = ReadResponse {
            session_id: 1,
            batches: Vec::new(),
            cursor: 9,
            gap: true,
            active: false,
            ended: Some("lease expired".to_string()),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let parsed: ReadResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, response);
    }
}
