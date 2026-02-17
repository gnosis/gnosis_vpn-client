pub use edgli::hopr_lib::{Address, NodeId, RoutingOptions};
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

    pub fn has_intermediate_channel(&self, address: Address) -> bool {
        match self.routing.clone() {
            RoutingOptions::Hops(_) => false,
            RoutingOptions::IntermediatePath(nodes) => nodes.into_iter().next().is_some_and(|node_id| match node_id {
                NodeId::Chain(addr) => addr == address,
                NodeId::Offchain(_) => false,
            }),
        }
    }

    pub fn pretty_print_path(&self) -> String {
        match self.routing.clone() {
            RoutingOptions::Hops(hops) => {
                let nr: u8 = hops.into();
                let path = (0..nr).map(|_| "()").collect::<Vec<&str>>().join("->");
                if nr > 0 {
                    format!("->{}->", path).to_string()
                } else {
                    "->".to_string()
                }
            }
            RoutingOptions::IntermediatePath(nodes) => {
                let path = nodes
                    .into_iter()
                    .map(|node_id| match node_id {
                        NodeId::Offchain(peer_id) => format!("({})", log_output::peer_id(peer_id.to_string().as_str())),
                        NodeId::Chain(address) => format!("({})", log_output::address(&address)),
                    })
                    .collect::<Vec<String>>()
                    .join("->");
                format!("->{}->", path).to_string()
            }
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
            address = self.address,
            short_addr = short_addr,
        )
    }
}
