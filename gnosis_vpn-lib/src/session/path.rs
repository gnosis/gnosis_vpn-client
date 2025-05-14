use std::cmp::PartialEq;
use std::fmt::{self, Display};

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
        match self
        write!(f, "WgRegistration[{}]", self.ip)
    }
}
