use std::fmt::{self, Display};

use edgli::hopr_lib::Address;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    pub node_address: Address,
    pub safe_address: Address,
    pub network: String,
}

impl Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Info(node_address: {}, safe_address: {}, network: {})",
            self.node_address, self.safe_address, self.network
        )
    }
}

impl Info {
    pub fn load_from(node: &edgli::hopr_lib::Hopr) -> Result<Self, edgli::hopr_lib::errors::HoprLibError> {
        Ok(Self {
            node_address: node.me_onchain(),
            safe_address: node.get_safe_config().safe_address,
            network: node.network(),
        })
    }
}
