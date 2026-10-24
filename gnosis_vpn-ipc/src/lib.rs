//! The service socket protocol and a blocking client; serde-light so user interfaces need no HOPR stack.
mod display;
mod protocol;
mod socket;
mod values;

pub use display::{META_FIELD_MAX_CHARS, elapsed, join, sanitize_for_display, scientific_suffix};
pub use protocol::*;
pub use socket::*;
pub use values::*;
