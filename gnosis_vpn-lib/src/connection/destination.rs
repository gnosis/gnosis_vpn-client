use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::log_output;
use crate::peer_id::PeerId;
use crate::session;

#[derive(Clone, Debug)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub peer_id: PeerId,
    pub path: session::Path,
}

impl Destination {
    pub fn new(peer_id: &PeerId, path: &session::Path, meta: &HashMap<String, String>) -> Self {
        Self {
            peer_id: peer_id.clone(),
            path: path.clone(),
            meta: meta.clone(),
        }
    }

    fn meta_str(&self) -> String {
        match self.meta.get("location") {
            Some(location) => {
                return location.clone();
            }
            None => self
                .meta
                .iter()
                .map(|(key, value)| format!("{}: {}", key, value))
                .collect::<Vec<String>>()
                .join(", "),
        }
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let peer_id = log_output::peer_id(&self.peer_id.to_string());
        let meta = self.meta_str();
        write!(f, "Destination[{},{}]", peer_id, meta)
    }
}
