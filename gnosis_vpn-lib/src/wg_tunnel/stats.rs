//! Snapshot type for NepTUN's own tunnel counters and timers.
//!
//! `Tunn::stats()` already tracks everything here internally; the pump loop
//! just samples it periodically and forwards it onward for `core` to retain a
//! short rolling window that the `nerd-stats` CLI command reads.

/// How many samples `core` retains per connection. At the pump's sampling
/// cadence this covers roughly the last 20 minutes.
pub(crate) const HISTORY_CAPACITY: usize = 300;

pub use gnosis_vpn_ipc::{TunnelStatsSample, WgTunnelStats};
