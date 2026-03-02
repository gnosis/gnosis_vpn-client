pub mod api;
pub mod errors;
pub mod types;
use edgli::DEFAULT_BLOKLI_URL;
pub use edgli::EdgliInitState;
pub use edgli::hopr_lib;
pub use edgli::hopr_lib::state::HoprState;
pub use {api::Hopr, errors::HoprError};

pub mod blokli_config;
pub mod config;
pub mod identity;

pub const ENV_VAR_CONFIG: &str = "GNOSISVPN_HOPR_CONFIG_PATH";
pub const ENV_VAR_ID_FILE: &str = "GNOSISVPN_HOPR_IDENTITY_FILE";
pub const ENV_VAR_ID_PASS: &str = "GNOSISVPN_HOPR_IDENTITY_PASS";
pub const ENV_VAR_BLOKLI_URL: &str = "GNOSISVPN_HOPR_BLOKLI_URL";

pub fn telemetry() -> Result<String, HoprError> {
    tracing::debug!("query hopr telemetry");
    edgli::hopr_lib::Hopr::<bool, bool>::collect_hopr_metrics().map_err(|e| HoprError::Telemetry(e.to_string()))
}

pub fn blokli_url(blokli_url: Option<url::Url>) -> url::Url {
    blokli_url.unwrap_or(DEFAULT_BLOKLI_URL.clone())
}
