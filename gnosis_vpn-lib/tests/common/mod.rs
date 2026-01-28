use gnosis_vpn_lib::event::WireGuardData;
use gnosis_vpn_lib::wireguard::{Config, InterfaceInfo, KeyPair, PeerInfo, WireGuard};
use std::net::Ipv4Addr;

/// Creates a test WireGuardData instance with customizable address and endpoint
pub fn create_test_wg_data(interface_addr: &str, peer_endpoint: &str) -> WireGuardData {
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
    WireGuardData {
        wg,
        interface_info: InterfaceInfo {
            address: interface_addr.to_string(),
        },
        peer_info: PeerInfo {
            public_key: "peer_key".to_string(),
            endpoint: peer_endpoint.to_string(),
        },
    }
}

/// Creates a simple test peer IP list
pub fn create_test_peer_ips(ips: &[u8]) -> Vec<Ipv4Addr> {
    ips.iter()
        .map(|&last_octet| Ipv4Addr::new(10, 0, 0, last_octet))
        .collect()
}
