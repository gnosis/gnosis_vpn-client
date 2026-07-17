pub use edgli::hopr_lib::HopRouting;
pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::exports::transport::SessionTarget;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};
use std::net::IpAddr;

use crate::log_output;
use crate::serde_utils;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: HashMap<String, String>,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: HopRouting,
    /// Exit-side address the ephemeral bridge session connects to.
    pub bridge_target: SessionTarget,
    /// Exit-side address the persistent wg (main tunnel) session connects to.
    pub wg_target: SessionTarget,
    /// Address pinged through the tunnel to validate connectivity/health.
    pub ping_address: IpAddr,
}

impl Destination {
    pub fn new(
        id: String,
        address: Address,
        routing: HopRouting,
        meta: HashMap<String, String>,
        bridge_target: SessionTarget,
        wg_target: SessionTarget,
        ping_address: IpAddr,
    ) -> Self {
        Self {
            id,
            address,
            routing,
            meta,
            bridge_target,
            wg_target,
            ping_address,
        }
    }

    pub fn pretty_print_path(&self) -> String {
        let nr = self.routing.hop_count();
        let path = (0..nr).map(|_| "()").collect::<Vec<&str>>().join("->");
        if nr > 0 {
            format!("->{path}->")
        } else {
            "->".to_string()
        }
    }

    fn meta_str(&self) -> String {
        let mut metas = self
            .meta
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<String>>();
        metas.sort_unstable();
        metas.join(", ")
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.meta.get(key).cloned()
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let short_addr = log_output::address(&self.address);
        write!(
            f,
            "{id} (Exit: {address}, Route: (entry){path}({short_addr}), {meta})",
            id = self.id,
            meta = self.meta_str(),
            path = self.pretty_print_path(),
            address = self.address.to_checksum(),
            short_addr = short_addr,
        )
    }
}
