use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display};
use std::ops::Range;

use crate::log_output;
use crate::monitor;
use crate::peer_id::PeerId;
use crate::session;

#[derive(Clone, Debug)]
pub struct Destination {
    pub meta: HashMap<String, String>,
    pub peer_id: PeerId,
    pub path: session::Path,
    pub bridge: SessionParameters,
    pub wg: SessionParameters,
    pub ping_interval: Range<u8>,
    pub ping_options: monitor::PingOptions,
}

#[derive(Clone, Debug)]
pub struct SessionParameters {
    pub target: session::Target,
    pub capabilities: Vec<session::Capability>,
}

impl SessionParameters {
    pub fn new(target: &session::Target, capabilities: &[session::Capability]) -> Self {
        Self {
            target: target.clone(),
            capabilities: capabilities.to_owned(),
        }
    }
}

impl Destination {
    pub fn new(
        peer_id: PeerId,
        path: session::Path,
        meta: HashMap<String, String>,
        bridge: SessionParameters,
        wg: SessionParameters,
        ping_interval: Range<u8>,
        ping_options: monitor::PingOptions,
    ) -> Self {
        Self {
            peer_id,
            path,
            meta,
            bridge,
            wg,
            ping_interval,
            ping_options,
        }
    }

    pub fn pretty_print_path(&self) -> String {
        format!(
            "{}(x{})",
            self.path,
            log_output::peer_id(self.peer_id.to_string().as_str())
        )
    }

    fn meta_str(&self) -> String {
        match self.meta.get("location") {
            Some(location) => location.clone(),
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
