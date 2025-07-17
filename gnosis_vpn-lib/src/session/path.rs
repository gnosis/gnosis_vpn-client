use serde::{Deserialize, Serialize};

use std::cmp::PartialEq;
use std::fmt::{self, Display};

use crate::address::Address;
use crate::log_output;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Path {
    Hops(u8),
    IntermediatePath(Vec<Address>),
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
                format!("->{h}->")
            }
            Path::IntermediatePath(intermediates) => {
                let i = intermediates
                    .iter()
                    .map(|address| format!("(r{})", log_output::address(address)))
                    .collect::<Vec<_>>()
                    .join("->");
                format!("->{i}->")
            }
        };
        write!(f, "{s}")
    }
}
