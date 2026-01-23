use gnosis_vpn_lib::event::{RequestToRoot, WireGuardData};
use gnosis_vpn_lib::wireguard::{Config, InterfaceInfo, KeyPair, PeerInfo, WireGuard};
use std::net::Ipv4Addr;

#[test]
fn dynamic_wg_routing_serializes_peer_ips() {
    let config = Config {
        listen_port: None,
        force_private_key: None,
        allowed_ips: None,
    };
    let key_pair = KeyPair {
        priv_key: "priv_key".to_string(),
        public_key: "pub_key".to_string(),
    };
    let wg = WireGuard::new(config, key_pair);
    let wg_data = WireGuardData {
        wg,
        interface_info: InterfaceInfo {
            address: "10.0.0.1/32".to_string(),
        },
        peer_info: PeerInfo {
            public_key: "peer_key".to_string(),
            endpoint: "127.0.0.1:51820".to_string(),
        },
    };
    let expected_peer_ips = vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2)];
    let request = RequestToRoot::DynamicWgRouting {
        wg_data,
        peer_ips: expected_peer_ips.clone(),
    };

    let serialized = serde_json::to_string(&request).expect("serialize RequestToRoot");
    let deserialized: RequestToRoot = serde_json::from_str(&serialized).expect("deserialize RequestToRoot");

    match deserialized {
        RequestToRoot::DynamicWgRouting { peer_ips, .. } => {
            assert_eq!(peer_ips, expected_peer_ips);
        }
        _ => panic!("unexpected request variant"),
    }
}
