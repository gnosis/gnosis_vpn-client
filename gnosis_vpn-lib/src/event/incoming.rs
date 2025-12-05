//! This module indicates external events that will be forwarded into core application loop.

use crate::command::Command;

#[derive(Debug, Clone)]
pub enum Incoming {
    Command(Command),
    Shutdown,
}
