use edgli::hopr_lib::Address;
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};

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
