//! Golden JSON for the socket protocol; every user interface parses exactly these shapes.
use gnosis_vpn_ipc::*;
use serde_json::{Value, json};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn at(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn address(byte: u8) -> Address {
    Address::new([byte; 20])
}

fn destination() -> Destination {
    Destination {
        id: "berlin".into(),
        meta: Meta::from_labels([("name".to_string(), "Berlin".to_string())]),
        geo: Some(Geo {
            country: "DE".parse().unwrap(),
            source: GeoSource::AnnouncedIp,
        }),
        address: address(1),
        hops: Hops::try_from(1).unwrap(),
        gnosis_vpn_server: "10.0.0.1:8000".parse().unwrap(),
        wireguard_server: "10.0.0.1:51820".parse().unwrap(),
        source: DestinationSource::Discovered,
        overrides: Overrides::default(),
    }
}

fn round_trips<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T, expected: Value) {
    let json = serde_json::to_value(value).expect("serialize");
    assert_eq!(json, expected);
    let back: T = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(serde_json::to_value(back).expect("re-serialize"), json);
}

#[test]
fn a_request_travels_in_a_versioned_envelope() {
    let envelope = Envelope {
        protocol: PROTOCOL_VERSION,
        request: Request::Connect {
            destination: "berlin".into(),
        },
    };
    round_trips(
        &envelope,
        json!({"protocol": 1, "request": {"type": "connect", "destination": "berlin"}}),
    );
}

#[test]
fn unit_requests_are_tagged_objects() {
    round_trips(&Request::Status, json!({"type": "status"}));
    round_trips(
        &Request::StartClient {
            timeout: Duration::from_secs(600),
        },
        json!({"type": "start_client", "timeout": 600_000}),
    );
}

#[test]
fn replies_carry_a_tagged_response_or_a_service_error() {
    round_trips(
        &Reply::Ok {
            response: Response::Pong,
        },
        json!({"type": "ok", "response": {"type": "pong"}}),
    );
    round_trips(
        &Reply::Err {
            error: ServiceError::ProtocolMismatch { service: 1 },
        },
        json!({"type": "err", "error": {"type": "protocol_mismatch", "service": 1}}),
    );
}

#[test]
fn a_destination_is_flat_with_typed_values() {
    round_trips(
        &destination(),
        json!({
            "id": "berlin",
            "meta": {"name": "Berlin", "description": null, "other": {}},
            "geo": {"country": "DE", "source": {"type": "announced_ip"}},
            "address": address(1).to_string(),
            "hops": 1,
            "gnosis_vpn_server": "10.0.0.1:8000",
            "wireguard_server": "10.0.0.1:51820",
            "source": {"type": "discovered"},
            "overrides": {"configured_meta": {}, "configured_gnosis_vpn_server": null, "configured_wireguard_server": null}
        }),
    );
}

#[test]
fn a_connect_response_nests_inside_the_adjacently_tagged_response() {
    let response = Response::Connect(ConnectResponse::DestinationAmbiguous {
        candidates: vec!["berlin-1".into(), "berlin-2".into()],
    });
    round_trips(
        &response,
        json!({"type": "connect", "data": {"type": "destination_ambiguous", "candidates": ["berlin-1", "berlin-2"]}}),
    );
}

#[test]
fn the_connection_is_one_tagged_state() {
    round_trips(&Connection::Disconnected, json!({"type": "disconnected"}));
    round_trips(
        &Connection::Reconnecting {
            destination: "berlin".into(),
            since: at(1_000),
            phase: None,
        },
        json!({"type": "reconnecting", "destination": "berlin", "since": 1_000, "phase": null}),
    );
    round_trips(
        &Connection::Connected {
            destination: "berlin".into(),
            since: at(2_000),
            tunnel_ping_rtt: Some(Duration::from_millis(90)),
            stall: Some(TunnelStall {
                since: at(3_000),
                failed_pings: 2,
                reconnect_at: 3,
            }),
        },
        json!({
            "type": "connected",
            "destination": "berlin",
            "since": 2_000,
            "tunnel_ping_rtt": 90,
            "stall": {"since": 3_000, "failed_pings": 2, "reconnect_at": 3}
        }),
    );
}

#[test]
fn warmup_reports_one_merged_stage() {
    let warmup = RunMode::Warmup {
        stage: WarmupStage::WaitingForFunds,
        last_error: None,
    };
    let json = serde_json::to_value(&warmup).expect("serialize");
    assert_eq!(
        json,
        json!({"type": "warmup", "stage": {"type": "waiting_for_funds"}, "last_error": null})
    );
}

#[test]
fn balances_are_wei_strings() {
    let balances = BalanceResponse::Available(Box::new(Balances {
        node: Balance::from_wei(1_500_000_000_000_000_000),
        safe: Balance::zero(),
        channels_out: vec![ChannelOut {
            address: address(2),
            balance: None,
            matched_exit: Some("berlin".into()),
        }],
        info: NodeInfo {
            node_address: address(3),
            node_peer_id: "12D3KooW".into(),
            safe_address: address(4),
        },
        capacity_allocations: None,
        ideal_balance: None,
        funding_status: Some(FundingStatus {
            traffic: FundingLevel::Low,
            gas: FundingLevel::Good,
            wxhopr_deficit: Some(Balance::from_wei(5)),
            xdai_deficit: None,
            refill_shortfall: None,
        }),
    }));
    let json = serde_json::to_value(&balances).expect("serialize");
    assert_eq!(json["type"], "available");
    assert_eq!(json["node"], "1500000000000000000");
    assert_eq!(json["safe"], "0");
    assert_eq!(json["channels_out"][0]["balance"], Value::Null);
    assert_eq!(json["funding_status"]["traffic"], json!({"type": "low"}));
    assert_eq!(json["funding_status"]["wxhopr_deficit"], "5");
}

#[test]
fn an_incompatible_exit_names_its_api_labels() {
    round_trips(
        &RouteHealthState::Unrecoverable {
            reason: UnrecoverableReason::IncompatibleApiVersion {
                offered: vec![ApiVersion::new(2)],
            },
        },
        json!({"type": "unrecoverable", "reason": {"type": "incompatible_api_version", "offered": ["v2"]}}),
    );
}

#[test]
fn a_route_walk_lists_relays_as_checksum_addresses() {
    round_trips(
        &RouteWalk::Paths {
            walked_at: at(5_000),
            count: 3,
            distinct_first_relays: 2,
            best_relays: vec![address(7)],
            best_value: 0.75,
            return_count: 2,
            return_best_relays: vec![],
            return_best_value: 0.5,
        },
        json!({
            "type": "paths",
            "walked_at": 5_000,
            "count": 3,
            "distinct_first_relays": 2,
            "best_relays": [address(7).to_string()],
            "best_value": 0.75,
            "return_count": 2,
            "return_best_relays": [],
            "return_best_value": 0.5
        }),
    );
}

#[test]
fn exit_versions_carry_api_labels_and_an_optional_server_version() {
    round_trips(
        &ExitVersions {
            api: vec![ApiVersion::new(1)],
            server: Some(semver::Version::new(0, 7, 1)),
        },
        json!({"api": ["v1"], "server": "0.7.1"}),
    );
}

#[test]
fn slots_name_the_ones_a_registration_still_holds() {
    let slots = |connected, available, total| {
        Slots {
            total,
            available,
            connected,
        }
        .to_string()
    };
    assert_eq!("slots: 1 / 16", slots(1, 15, 16));
    assert_eq!("slots: 1(5 pending) / 16", slots(1, 10, 16));
    // A server reporting more in use than it has must not panic the status output.
    assert_eq!("slots: 9 / 8", slots(9, 4, 8));
}

#[test]
fn a_stall_shows_only_while_pings_fail() {
    let connected = |stall| Connection::Connected {
        destination: "berlin".into(),
        since: UNIX_EPOCH,
        tunnel_ping_rtt: Some(Duration::from_millis(90)),
        stall,
    };
    let healthy = connected(None).to_string();
    assert!(!healthy.contains("stalled"), "{healthy}");
    let stalled = connected(Some(TunnelStall {
        since: UNIX_EPOCH,
        failed_pings: 2,
        reconnect_at: 3,
    }))
    .to_string();
    assert!(stalled.ends_with("(2/3)"), "{stalled}");
}

#[test]
fn a_reconnect_without_attempt_names_the_wait() {
    let waiting = Connection::Reconnecting {
        destination: "berlin".into(),
        since: UNIX_EPOCH,
        phase: None,
    }
    .to_string();
    assert!(waiting.contains("waiting for route health"), "{waiting}");
}
