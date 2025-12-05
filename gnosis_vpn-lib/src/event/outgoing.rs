//! This module indicates internal events that come from the core application loop.

#[derive(Debug, Clone)]
pub enum Outgoing {
    WgUp(String),
    WgDown,
}
