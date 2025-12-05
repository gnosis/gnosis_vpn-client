//! This module indicates internal events that come from the core application loop.

use crate::command::Response;

#[derive(Debug, Clone)]
pub enum Outgoing {
    Response(Box<Response>),
}
