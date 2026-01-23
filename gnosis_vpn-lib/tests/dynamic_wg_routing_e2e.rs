use gnosis_vpn_lib::event::{CoreToWorker, RequestToRoot, WireGuardData};
use gnosis_vpn_lib::wireguard::{Config, InterfaceInfo, KeyPair, PeerInfo, WireGuard};
use std::net::Ipv4Addr;

#[test]
fn dynamic_wg_routing_round_trips_through_core_message() {
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
            address: "10.0.0.2/32".to_string(),
        },
        peer_info: PeerInfo {
            public_key: "peer_key".to_string(),
            endpoint: "127.0.0.1:51821".to_string(),
        },
    };
    let expected_peer_ips = vec![Ipv4Addr::new(10, 0, 0, 9)];
    let request = RequestToRoot::DynamicWgRouting {
        wg_data,
        peer_ips: expected_peer_ips.clone(),
    };
    let message = CoreToWorker::RequestToRoot(request);

    match message {
        CoreToWorker::RequestToRoot(RequestToRoot::DynamicWgRouting { peer_ips, .. }) => {
            assert_eq!(peer_ips, expected_peer_ips);
        }
        _ => panic!("unexpected core message"),
    }
}
