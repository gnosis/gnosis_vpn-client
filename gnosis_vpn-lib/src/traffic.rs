//! Wire contract for the opt-in public-interface traffic capture.
//!
//! The root daemon captures IP traffic on one explicitly selected interface and
//! publishes one-second aggregates. Aggregation lives in the daemon so that
//! neither packet payloads nor per-packet events ever leave the privileged
//! process. Presentation state (history retention, row assignment, viewport
//! projection) belongs to the client application, not here.
//!
//! Capture only describes what is observable on the wire: a target is either
//! associated with an announced HOPR peer address or it is not. The daemon makes
//! no claim about which process sent a packet, and it cannot see payload that a
//! tunnel carries.

use serde::{Deserialize, Serialize};

use std::net::IpAddr;

/// Bumped on any incompatible change to the types below. Consumers must refuse
/// to interpret a batch carrying an unknown version.
pub const PROTOCOL_VERSION: u16 = 1;

/// One interface the daemon is willing to capture on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    /// Stable capture identifier, accepted back by [`Command::TrafficStart`].
    pub id: String,
    /// Human-readable name for the interface picker.
    pub name: String,
    /// Usable local addresses; used by the daemon to decide packet direction.
    pub addresses: Vec<IpAddr>,
    /// Whether the host's egress route currently uses this interface.
    pub is_default: bool,
}

/// How a captured packet relates to the VPN transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// The remote address is one announced by the HOPR node, i.e. the traffic is
    /// plausibly part of the mixnet transport. Association, not ownership.
    HoprAssociated,
    /// No association rule matched. May be ordinary traffic, RPC traffic, an
    /// unrelated VPN, or mixnet traffic to an address that is not announced.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Host to target.
    Upstream,
    /// Target to host.
    Downstream,
}

/// Aggregated traffic for one target, class, direction and second.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    /// Whole seconds since the capture session started.
    pub second: u64,
    pub target: IpAddr,
    pub class: Class,
    pub direction: Direction,
    pub packets: u32,
    /// Sum of complete IP-layer lengths, including the IP header.
    pub bytes: u64,
    pub largest_packet: u32,
}

/// A bounded slice of aggregated traffic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub protocol: u16,
    pub session_id: u64,
    /// Consecutive per session, starting at 0.
    pub sequence: u64,
    /// Whole milliseconds since the capture session started. Advances even when
    /// the session carried no traffic, so consumers can age their view.
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
    /// The requested cursor is older than the retained window; batches were lost
    /// and the consumer must treat the gap as missing data rather than as zero.
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

/// Replaces the announced-peer address set used for [`Class::HoprAssociated`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAddresses {
    pub addresses: Vec<IpAddr>,
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
                class: Class::HoprAssociated,
                direction: Direction::Downstream,
                packets: 4,
                bytes: 5120,
                largest_packet: 1400,
            }],
            dropped_batches: 0,
        };
        let json = serde_json::to_string(&batch).expect("serialize");
        assert!(json.contains("\"class\":\"hopr_associated\""), "{json}");
        assert!(json.contains("\"direction\":\"downstream\""), "{json}");
        let parsed: Batch = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, batch);
    }

    #[test]
    fn commands_use_the_wire_format_the_app_sends() {
        use crate::command::Command;
        let start = Command::TrafficStart(StartRequest {
            interface_id: "en0".to_string(),
        });
        assert_eq!(
            r#"{"TrafficStart":{"interface_id":"en0"}}"#,
            serde_json::to_string(&start).expect("serialize")
        );
        let read = Command::TrafficRead(ReadRequest {
            session_id: 2,
            cursor: 7,
        });
        assert_eq!(
            r#"{"TrafficRead":{"session_id":2,"cursor":7}}"#,
            serde_json::to_string(&read).expect("serialize")
        );
        let stop = Command::TrafficStop(StopRequest { session_id: 2 });
        assert_eq!(
            r#"{"TrafficStop":{"session_id":2}}"#,
            serde_json::to_string(&stop).expect("serialize")
        );
        assert_eq!(
            r#""TrafficInterfaces""#,
            serde_json::to_string(&Command::TrafficInterfaces).expect("serialize")
        );
    }

    #[test]
    fn responses_use_the_wire_format_the_app_reads() {
        use crate::command::Response;
        let started = Response::TrafficStart(Ok(StartResponse {
            session_id: 1,
            interface: Interface {
                id: "en0".to_string(),
                name: "en0".to_string(),
                addresses: vec!["192.0.2.2".parse().expect("ip")],
                is_default: true,
            },
            protocol: PROTOCOL_VERSION,
        }));
        assert_eq!(
            r#"{"TrafficStart":{"Ok":{"session_id":1,"interface":{"id":"en0","name":"en0","addresses":["192.0.2.2"],"is_default":true},"protocol":1}}}"#,
            serde_json::to_string(&started).expect("serialize")
        );
        assert_eq!(
            r#"{"TrafficStart":{"Err":"Interface wg0 is not available"}}"#,
            serde_json::to_string(&Response::TrafficStart(Err(
                "Interface wg0 is not available".to_string()
            )))
            .expect("serialize")
        );
        assert_eq!(
            r#""TrafficDenied""#,
            serde_json::to_string(&Response::TrafficDenied).expect("serialize")
        );
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
