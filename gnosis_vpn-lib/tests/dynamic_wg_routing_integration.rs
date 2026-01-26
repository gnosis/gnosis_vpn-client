mod common;

use gnosis_vpn_lib::event::RequestToRoot;

#[test]
fn dynamic_wg_routing_serializes_peer_ips() {
    let wg_data = common::create_test_wg_data("10.0.0.1/32", "127.0.0.1:51820");
    let expected_peer_ips = common::create_test_peer_ips(&[1, 2]);
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
