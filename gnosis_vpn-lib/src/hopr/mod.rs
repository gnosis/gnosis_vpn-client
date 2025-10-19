pub mod api;
mod errors;
mod types;
pub use edgli::hopr_lib::state::HoprState;
pub use {api::Hopr, errors::HoprError};

pub mod config;
pub mod identity;

pub const CONFIG_ENV: &str = "GNOSISVPN_HOPR_CONFIG_PATH";
pub const ID_FILE_ENV: &str = "GNOSISVPN_HOPR_IDENTITY_FILE";
pub const ID_PASS_ENV: &str = "GNOSISVPN_HOPR_IDENTITY_PASS";
pub const RPC_PROVIDER_ENV: &str = "GNOSISVPN_HOPR_RPC_PROVIDER";
pub const NETWORK_ENV: &str = "GNOSISVPN_HOPR_NETWORK";
