mod api;
mod errors;
mod types;
pub use {api::Hopr, errors::HoprError};

pub mod config;
pub mod identity;

pub const CONFIG_ENV: &str = "GNOSISVPN_HOPR_CONFIG_PATH";
pub const ID_FILE_ENV: &str = "GNOSISVPN_HOPR_IDENTITY_FILE";
pub const ID_PASS_ENV: &str = "GNOSISVPN_HOPR_IDENTITY_PASS";
pub const DB_ENV: &str = "GNOSISVPN_HOPR_DB_PATH";
