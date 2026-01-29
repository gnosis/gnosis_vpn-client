pub mod api;
pub mod errors;
pub mod types;
pub use edgli::hopr_lib;
pub use edgli::hopr_lib::state::HoprState;
pub use {api::Hopr, errors::HoprError};

pub mod config;
pub mod identity;

pub const ENV_VAR_CONFIG: &str = "GNOSISVPN_HOPR_CONFIG_PATH";
pub const ENV_VAR_ID_FILE: &str = "GNOSISVPN_HOPR_IDENTITY_FILE";
pub const ENV_VAR_ID_PASS: &str = "GNOSISVPN_HOPR_IDENTITY_PASS";
pub const ENV_VAR_BLOKLI_URL: &str = "GNOSISVPN_HOPR_BLOKLI_URL";
