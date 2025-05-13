use std::cmp::PartialEq;

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
