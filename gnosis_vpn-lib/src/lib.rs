pub mod app_nap;
pub mod balance;
pub mod check_update;
pub mod command;
pub mod compat;
pub mod config;
pub mod connection;
pub mod core;
pub mod dirs;
pub mod event;
pub mod gvpn_client;
pub mod hopr;
pub mod info;
pub mod logging;
pub mod ping;
pub mod route_health;
pub mod shell_command_ext;
pub mod socket;
pub mod wireguard;
pub mod worker;
pub mod worker_params;

mod log_output;
mod peer;
mod remote_data;
mod ticket_stats;

pub mod prelude {
    pub use edgli::hopr_lib::Address;
}

pub const IDENTIFIER: &str = "com.gnosisvpn.gnosisvpnclient";
