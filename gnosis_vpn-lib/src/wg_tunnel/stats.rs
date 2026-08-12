//! Snapshot and bounded history of NepTUN's own tunnel counters and timers.
//!
//! `Tunn::stats()` already tracks everything here internally; the pump loop
//! just samples it periodically and keeps a short rolling window for the
//! `nerd-stats` CLI command to read.

use serde::{Deserialize, Serialize};

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::serde_utils;

/// How many samples to retain. At the pump's sampling cadence this covers
/// roughly the last 20 minutes.
const HISTORY_CAPACITY: usize = 300;

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

/// Shared handle the pump loop records into and the query path reads from.
///
/// Deliberately separate from the engine's own state (see `pump`'s module doc
/// on owning the engine without a mutex): this lock is only touched a few
/// times a minute, never on the packet hot path, and its critical section
/// never panics, so a poisoned lock is recovered rather than propagated.
#[derive(Debug, Default)]
pub struct WgTunnelStatsHandle(Mutex<VecDeque<TunnelStatsSample>>);

impl WgTunnelStatsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new sample, evicting the oldest once at capacity.
    pub fn record(&self, sample: TunnelStatsSample) {
        let mut history = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if history.len() >= HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(sample);
    }

    /// Clone out the latest sample plus the full retained history.
    pub fn snapshot(&self) -> WgTunnelStats {
        let history = self.0.lock().unwrap_or_else(|e| e.into_inner());
        WgTunnelStats {
            current: history.back().cloned(),
            history: history.iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(tx_bytes: u64) -> TunnelStatsSample {
        TunnelStatsSample {
            tx_bytes,
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_is_empty_before_any_record() {
        let handle = WgTunnelStatsHandle::new();
        let stats = handle.snapshot();
        assert!(stats.current.is_none());
        assert!(stats.history.is_empty());
    }

    #[test]
    fn record_appends_and_snapshot_reflects_latest() {
        let handle = WgTunnelStatsHandle::new();
        handle.record(sample(1));
        handle.record(sample(2));
        let stats = handle.snapshot();
        assert_eq!(stats.current, Some(sample(2)));
        assert_eq!(stats.history, vec![sample(1), sample(2)]);
    }

    #[test]
    fn record_evicts_oldest_past_capacity() {
        let handle = WgTunnelStatsHandle::new();
        for i in 0..(HISTORY_CAPACITY + 5) {
            handle.record(sample(i as u64));
        }
        let stats = handle.snapshot();
        assert_eq!(stats.history.len(), HISTORY_CAPACITY);
        assert_eq!(stats.history.first().unwrap().tx_bytes, 5);
        assert_eq!(stats.history.last().unwrap().tx_bytes, (HISTORY_CAPACITY + 4) as u64);
    }
}
