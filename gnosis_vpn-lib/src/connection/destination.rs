pub use edgli::hopr_lib::HopRouting;
pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::log_output;
use crate::serde_utils;

#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RoutingMode {
    HopBased(HopRouting),
    ExplicitPath(#[serde_as(as = "Vec<DisplayFromStr>")] Vec<Address>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: HashMap<String, String>,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: RoutingMode,
}

impl Destination {
    pub fn new(id: String, address: Address, routing: RoutingMode, meta: HashMap<String, String>) -> Self {
        Self {
            id,
            address,
            routing,
            meta,
        }
    }

    pub fn pretty_print_path(&self) -> String {
        match &self.routing {
            RoutingMode::HopBased(hop_routing) => {
                let nr = hop_routing.hop_count();
                let path = (0..nr).map(|_| "()").collect::<Vec<&str>>().join("->");
                if nr > 0 { format!("->{path}->") } else { "->".to_string() }
            }
            RoutingMode::ExplicitPath(intermediates) => {
                let path = intermediates
                    .iter()
                    .map(|a| format!("({})", log_output::address(a)))
                    .collect::<Vec<_>>()
                    .join("->");
                if path.is_empty() { "->".to_string() } else { format!("->{path}->") }
            }
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
