//! Snapshot type for NepTUN's own tunnel counters and timers.
//!
//! `Tunn::stats()` already tracks everything here internally; the pump loop
//! just samples it periodically and forwards it onward for `core` to retain a
//! short rolling window that the `nerd-stats` CLI command reads.

use serde::{Deserialize, Serialize};

use std::time::{Duration, SystemTime};

use crate::serde_utils;

/// How many samples `core` retains per connection. At the pump's sampling
/// cadence this covers roughly the last 20 minutes.
pub(crate) const HISTORY_CAPACITY: usize = 300;

/// A point-in-time snapshot of NepTUN's internal tunnel counters and timers.
///
/// `rtt_ms` and `time_since_last_handshake` only advance on a WireGuard
/// handshake, which recurs roughly every two minutes on an active tunnel
/// (`REKEY_AFTER_TIME` in neptun) - they reflect the state as of the last
/// handshake, not "now".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TunnelStatsSample {
    #[serde(with = "serde_utils::system_time")]
    pub at: SystemTime,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub rtt_ms: Option<u32>,
    #[serde(with = "serde_utils::opt_duration_ms")]
    pub time_since_last_handshake: Option<Duration>,
    pub estimated_loss: f32,
}

impl Default for TunnelStatsSample {
    fn default() -> Self {
        Self {
            at: SystemTime::UNIX_EPOCH,
            tx_bytes: 0,
            rx_bytes: 0,
            rtt_ms: None,
            time_since_last_handshake: None,
            estimated_loss: 0.0,
        }
    }
}

/// The latest sample plus a bounded window of history, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WgTunnelStats {
    pub current: Option<TunnelStatsSample>,
    pub history: Vec<TunnelStatsSample>,
}
