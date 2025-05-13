use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display};

use crate::log_output;
use crate::peer_id::PeerId;
use crate::session;

#[derive(Clone, Debug)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub peer_id: PeerId,
    pub path: session::Path,
    pub bridge: SessionParameters,
    pub wg: SessionParameters,
}

#[derive(Clone, Debug)]
pub struct SessionParameters {
    pub target: session::Target,
    pub capabilities: Vec<session::Capability>,
}

impl SessionParameters {
    pub fn new(target: &session::Target, capabilities: &Vec<session::Capability>) -> Self {
        Self {
            target: target.clone(),
            capabilities: capabilities.clone(),
        }
    }
}

impl Destination {
    pub fn new(
        peer_id: &PeerId,
        path: &session::Path,
        meta: &HashMap<String, String>,
        bridge: &SessionParameters,
        wg: &SessionParameters,
    ) -> Self {
        Self {
            peer_id: peer_id.clone(),
            path: path.clone(),
            meta: meta.clone(),
            bridge: bridge.clone(),
            wg: wg.clone(),
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

impl PartialEq for Destination {
    fn eq(&self, other: &Self) -> bool {
        self.peer_id == other.peer_id && self.path == other.path && self.bridge == other.bridge && self.wg == other.wg
    }
}

impl PartialEq for SessionParameters {
    fn eq(&self, other: &Self) -> bool {
        let left_set: HashSet<_> = self.capabilities.iter().collect();
        let right_set: HashSet<_> = other.capabilities.iter().collect();
        self.target == other.target && left_set == right_set
    }
}
