use std::cmp::PartialEq;
use std::fmt::{self, Display};

use crate::log_output;
use crate::peer_id::PeerId;

#[derive(Clone, Debug, PartialEq)]
pub enum Path {
    Hops(u8),
    Intermediates(Vec<PeerId>),
}

impl Default for Path {
    fn default() -> Self {
        Path::Hops(1)
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s: String = match self {
            Path::Hops(hops) => (1..*hops).map(|_| "()").collect::<Vec<_>>().join("->"),
            Path::Intermediates(intermediates) => intermediates
                .iter()
                .map(|peer_id| format!("({})", log_output::peer_id(peer_id.to_string().as_str())))
                .collect::<Vec<_>>()
                .join("->"),
        };
        write!(f, "{}", s)
    }
}
