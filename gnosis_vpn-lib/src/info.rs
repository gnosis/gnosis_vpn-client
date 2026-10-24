use edgli::hopr_lib::api::types::primitive::prelude::Address;

use std::fmt::{self, Display};

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub node_address: Address,
    pub node_peer_id: String,
    pub safe_address: Address,
}

impl Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Info(node_address: {}, safe_address: {})",
            self.node_address.to_checksum(),
            self.safe_address.to_checksum()
        )
    }
}

impl From<&Info> for gnosis_vpn_ipc::NodeInfo {
    fn from(info: &Info) -> Self {
        Self {
            node_address: crate::wire::address(&info.node_address),
            node_peer_id: info.node_peer_id.as_str().into(),
            safe_address: crate::wire::address(&info.safe_address),
        }
    }
}
