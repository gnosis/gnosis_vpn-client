pub use edgli::hopr_lib::api::types::internal::routing::RoutingOptions;
pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::log_output;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: HashMap<String, String>,
    pub address: Address,
    pub routing: RoutingOptions,
}

impl Destination {
    pub fn new(id: String, address: Address, routing: RoutingOptions, meta: HashMap<String, String>) -> Self {
        Self {
            id,
            address,
            routing,
            meta,
        }
    }

    pub fn has_intermediate_channel(&self, _address: Address) -> bool {
        false
    }

    pub fn pretty_print_path(&self) -> String {
        let nr: u8 = match self.routing.clone() {
            RoutingOptions::Hops(hops) => hops.into(),
            RoutingOptions::IntermediatePath(p) => p.as_ref().len() as u8,
        };
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
        metas.sort();
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
