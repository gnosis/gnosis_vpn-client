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
            Path::Hops(0) => "->".to_string(),
            Path::Hops(hops) => {
                let h = (1..*hops).map(|_| "()").collect::<Vec<_>>().join("->");
                format!("->{}->", h)
            }
            Path::Intermediates(intermediates) => {
                let i = intermediates
                    .iter()
                    .map(|peer_id| format!("(r{})", log_output::peer_id(peer_id.to_string().as_str())))
                    .collect::<Vec<_>>()
                    .join("->");
                format!("->{}->", i)
            }
        };
        write!(f, "{}", s)
    }
}
