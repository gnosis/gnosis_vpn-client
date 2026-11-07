//! PIX chunks per session from hopr's events; payment and usage are telemetry deltas since the first chunk.

use edgli::hopr_lib::api::node::{HasExitIncentivization, PixEvent};
use edgli::hopr_lib::api::types::primitive::prelude::HoprBalance;
use edgli::hopr_lib::exports::transport::session::PIX_QUOTA_BYTES_PER_SHARE;
use futures_util::StreamExt;
use gnosis_vpn_ipc as ipc;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hopr::metrics::{gauge_u64, gauge_value, unlabeled_u64};
use crate::wire;

/// Only the live main session is ever looked up; older ones linger until pushed out.
const MAX_SESSIONS: usize = 16;

const SURBS_PRODUCED: &str = "hopr_session_surb_produced_total";
const SURB_RATE: &str = "hopr_session_surb_rate_per_sec";

#[derive(Clone, Default)]
pub struct PixChunks(Arc<Mutex<VecDeque<SessionChunks>>>);

#[derive(Clone, Debug, PartialEq)]
struct SessionChunks {
    session_id: String,
    committed: u32,
    chunk_bytes: u64,
    /// SURBs minted before the first chunk carried no share, so they are not used quota.
    surbs_before_first_chunk: u64,
    deposits_before_first_chunk: Deposits,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Deposits {
    sent: u64,
    failed: u64,
}

impl Deposits {
    fn read(telemetry: &str) -> Self {
        // a counter is missing until its first increment
        let count = |metric| unlabeled_u64(telemetry, metric).unwrap_or(0);
        Self {
            sent: count("hopr_strategy_pix_deposits_total"),
            failed: count("hopr_strategy_pix_deposits_failed_total")
                + count("hopr_strategy_pix_deposits_rejected_total")
                + count("hopr_strategy_pix_deposits_over_budget_total"),
        }
    }
}

impl PixChunks {
    /// Subscribe before any session opens: events sent before subscribing are lost.
    pub fn listen(hopr: &impl HasExitIncentivization) -> Self {
        let chunks = Self::default();
        let ledger = chunks.clone();
        // the broadcast drops its oldest events instead of blocking, so this can't stall the strategy
        let mut events = Box::pin(hopr.subscribe_pix_events());
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let PixEvent::NewDepositAddress(chunk) = event else {
                    continue;
                };
                let telemetry = super::telemetry()
                    .inspect_err(|err| tracing::warn!(%err, "no telemetry for PIX chunk baseline"))
                    .unwrap_or_default();
                ledger.record(
                    chunk.id.session_id().as_ref(),
                    chunk.id.ssa_index().get(),
                    chunk.quota,
                    &telemetry,
                );
            }
        });
        chunks
    }

    fn record(&self, session_id: &str, chunk_index: u32, chunk_bytes: u64, telemetry: &str) {
        tracing::debug!(%session_id, chunk_index, chunk_bytes, "exit asked for a PIX chunk");
        let mut sessions = self.0.lock().expect("pix chunks lock poisoned");
        if let Some(known) = sessions.iter_mut().find(|s| s.session_id == session_id) {
            known.committed = known.committed.max(chunk_index);
            known.chunk_bytes = chunk_bytes;
            return;
        }
        if sessions.len() == MAX_SESSIONS {
            sessions.pop_front();
        }
        sessions.push_back(SessionChunks {
            session_id: session_id.to_string(),
            committed: chunk_index,
            chunk_bytes,
            surbs_before_first_chunk: gauge_u64(telemetry, SURBS_PRODUCED, session_id).unwrap_or(0),
            deposits_before_first_chunk: Deposits::read(telemetry),
        });
    }

    pub fn stats(&self, session_id: &str, telemetry: &str, price_per_byte: HoprBalance) -> ipc::PixChunkStats {
        let sessions = self.0.lock().expect("pix chunks lock poisoned");
        let Some(session) = sessions.iter().find(|s| s.session_id == session_id) else {
            return ipc::PixChunkStats {
                phase: ipc::PixPhase::Free,
                chunks_committed: 0,
                chunks_paid: 0,
                chunks_failed: 0,
                chunk_bytes: 0,
                bytes_bought: 0,
                cost: wire::wxhopr(HoprBalance::zero()),
                bytes_used: None,
                bytes_left: None,
                empty_in: None,
            };
        };

        let committed = u64::from(session.committed);
        let now = Deposits::read(telemetry);
        let before = session.deposits_before_first_chunk;
        let paid = now.sent.saturating_sub(before.sent).min(committed);
        let failed = now.failed.saturating_sub(before.failed);
        let phase = if failed > 0 {
            ipc::PixPhase::PaymentFailed
        } else if paid < committed {
            ipc::PixPhase::PaymentPending
        } else {
            ipc::PixPhase::Funded
        };

        let bytes_bought = committed * session.chunk_bytes;
        let bytes_used = gauge_u64(telemetry, SURBS_PRODUCED, session_id)
            .map(|produced| produced.saturating_sub(session.surbs_before_first_chunk) * PIX_QUOTA_BYTES_PER_SHARE);
        let bytes_left = bytes_used.map(|used| bytes_bought.saturating_sub(used));
        let bytes_per_sec = gauge_value(telemetry, SURB_RATE, session_id)
            .map(|surbs| surbs * PIX_QUOTA_BYTES_PER_SHARE as f64)
            .filter(|rate| *rate > 0.0);
        let empty_in = match (bytes_left, bytes_per_sec) {
            (Some(left), Some(rate)) => Some(Duration::from_secs_f64(left as f64 / rate)),
            _ => None,
        };

        ipc::PixChunkStats {
            phase,
            chunks_committed: session.committed,
            chunks_paid: paid,
            chunks_failed: failed,
            chunk_bytes: session.chunk_bytes,
            bytes_bought,
            cost: wire::wxhopr(price_per_byte * bytes_bought),
            bytes_used,
            bytes_left,
            empty_in,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: u64 = 951_644_160;

    fn telemetry(surbs: u64, rate: f64, sent: u64, failed: u64) -> String {
        format!(
            "{SURBS_PRODUCED}{{session_id=\"s1\"}} {surbs}\n\
             {SURB_RATE}{{session_id=\"s1\"}} {rate}\n\
             hopr_strategy_pix_deposits_total {sent}\n\
             hopr_strategy_pix_deposits_failed_total {failed}\n"
        )
    }

    fn price() -> HoprBalance {
        HoprBalance::from(1u64)
    }

    #[test]
    fn a_session_without_chunks_is_free() {
        let stats = PixChunks::default().stats("s1", &telemetry(500, 10.0, 0, 0), price());
        assert_eq!(stats.phase, ipc::PixPhase::Free);
        assert_eq!(stats.bytes_bought, 0);
    }

    #[test]
    fn a_chunk_is_pending_until_its_deposit_is_sent() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 10.0, 4, 0));

        let pending = chunks.stats("s1", &telemetry(1_100, 10.0, 4, 0), price());
        assert_eq!(pending.phase, ipc::PixPhase::PaymentPending);

        let funded = chunks.stats("s1", &telemetry(1_100, 10.0, 5, 0), price());
        assert_eq!(funded.phase, ipc::PixPhase::Funded);
        assert_eq!(funded.chunks_paid, 1);
    }

    #[test]
    fn a_failed_payment_wins_over_paid_chunks() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));

        let stats = chunks.stats("s1", &telemetry(0, 0.0, 1, 1), price());
        assert_eq!(stats.phase, ipc::PixPhase::PaymentFailed);
        assert_eq!(stats.chunks_committed, 2);
        assert_eq!((stats.chunks_paid, stats.chunks_failed), (1, 1));
    }

    #[test]
    fn usage_counts_only_surbs_minted_after_the_first_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 0.0, 0, 0));

        let stats = chunks.stats("s1", &telemetry(1_100, 100.0, 1, 0), price());
        let used = 100 * PIX_QUOTA_BYTES_PER_SHARE;
        assert_eq!(stats.bytes_used, Some(used));
        assert_eq!(stats.bytes_left, Some(CHUNK - used));
        let secs = (CHUNK - used) as f64 / (100.0 * PIX_QUOTA_BYTES_PER_SHARE as f64);
        assert_eq!(stats.empty_in, Some(Duration::from_secs_f64(secs)));
    }

    #[test]
    fn later_chunks_add_to_what_was_bought() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 3, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));

        let stats = chunks.stats("s1", &telemetry(0, 0.0, 3, 0), price());
        assert_eq!(stats.chunks_committed, 3);
        assert_eq!(stats.bytes_bought, 3 * CHUNK);
        assert_eq!(stats.cost, wire::wxhopr(HoprBalance::from(3 * CHUNK)));
    }

    #[test]
    fn old_sessions_are_pushed_out() {
        let chunks = PixChunks::default();
        for i in 0..=MAX_SESSIONS {
            chunks.record(&format!("old{i}"), 1, CHUNK, "");
        }
        assert_eq!(chunks.stats("old0", "", price()).phase, ipc::PixPhase::Free);
        assert_ne!(
            chunks.stats(&format!("old{MAX_SESSIONS}"), "", price()).phase,
            ipc::PixPhase::Free
        );
    }
}
