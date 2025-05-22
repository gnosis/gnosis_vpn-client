use serde::Serialize;

#[derive(Clone, Debug, Serialize, Hash)]
pub enum Capability {
    Segmentation,
    Retransmission,
}
