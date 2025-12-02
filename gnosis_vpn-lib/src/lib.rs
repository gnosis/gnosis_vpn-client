pub mod balance;
pub mod chain;
pub mod command;
pub mod config;
pub mod connection;
pub mod core;
pub mod external_event;
pub mod hopr;
pub mod hopr_params;
pub mod info;
pub mod network;
pub mod routing;
pub mod socket;
pub mod wg_tooling;
pub mod worker;
pub mod worker_command;

mod dirs;
mod gvpn_client;
mod log_output;
mod ping;
mod remote_data;
mod ticket_stats;

pub mod prelude {
    pub use edgli::hopr_lib::Address;
}

pub const IDENTIFIER: &str = "com.gnosisvpn.client";
