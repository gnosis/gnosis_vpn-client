use std::collections::HashMap;
use std::fmt::{self, Display};

use edgli::hopr_lib::Address;

use crate::log_output;
use crate::session;

#[derive(Clone, Debug)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub address: Address,
    pub path: session::Path,
}

impl Destination {
    pub fn new(address: Address, path: session::Path, meta: HashMap<String, String>) -> Self {
        Self { address, path, meta }
    }

    pub fn pretty_print_path(&self) -> String {
        format!("{}({})", self.path, log_output::address(&self.address))
    }

    fn meta_str(&self) -> String {
        match self.meta.get("location") {
            Some(location) => location.clone(),
            None => self
                .meta
                .iter()
                .map(|(key, value)| format!("{key}: {value}"))
                .collect::<Vec<String>>()
                .join(", "),
        }
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let address = log_output::address(&self.address);
        let meta = self.meta_str();
        write!(f, "Destination[{address},{meta}]")
    }
}
