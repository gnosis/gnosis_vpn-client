pub mod killswitch;

pub mod app_nap;
pub mod balance;
pub mod check_update;
pub mod command;
pub mod config;
pub mod connection;
pub mod core;
pub mod dirs;
pub mod event;
pub mod hopr;
pub mod logging;
pub mod ping;
pub mod route_health;
pub mod shell_command_ext;
pub mod socket;
pub mod update;
#[cfg(target_os = "linux")]
pub mod update_apt;
pub mod wireguard;
pub mod worker;
pub mod worker_params;

pub mod prelude {
    pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
}

pub const IDENTIFIER: &str = "com.gnosisvpn.gnosisvpnclient";

pub(crate) mod compat;
pub(crate) mod gvpn_client;
pub(crate) mod info;
pub(crate) mod ticket_stats;

mod log_output;
mod peer;
mod remote_data;
mod serde_utils;
