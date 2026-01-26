mod common;

use gnosis_vpn_lib::event::{CoreToWorker, RequestToRoot};

#[test]
fn dynamic_wg_routing_round_trips_through_core_message() {
    let wg_data = common::create_test_wg_data("10.0.0.2/32", "127.0.0.1:51821");
    let expected_peer_ips = common::create_test_peer_ips(&[9]);
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
