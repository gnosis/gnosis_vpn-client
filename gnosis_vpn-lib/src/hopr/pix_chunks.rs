//! PIX chunks per session from hopr's events; payment and usage are telemetry deltas since the first chunk.

use edgli::hopr_lib::api::node::{HasExitIncentivization, PixEvent};
use edgli::hopr_lib::api::types::primitive::prelude::HoprBalance;
use edgli::hopr_lib::exports::transport::session::PIX_QUOTA_BYTES_PER_SHARE;
use futures_util::StreamExt;
use gnosis_vpn_ipc as ipc;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hopr::metrics::{gauge_u64, gauge_value, unlabeled_u64};
use crate::wire;

/// Only the live main session is ever looked up; older ones linger until pushed out.
const MAX_SESSIONS: usize = 16;

/// Status is polled every few seconds and a scrape serializes the whole metrics registry.
const TELEMETRY_MAX_AGE: Duration = Duration::from_secs(5);

const SURBS_PRODUCED: &str = "hopr_session_surb_produced_total";
const SURB_RATE: &str = "hopr_session_surb_rate_per_sec";

#[derive(Clone, Default)]
pub struct PixChunks {
    sessions: Arc<Mutex<VecDeque<SessionChunks>>>,
    telemetry: Arc<Mutex<Option<(Instant, String)>>>,
}

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
        let mut sessions = self.sessions.lock().expect("pix chunks lock poisoned");
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

    /// Reads telemetry only once the session has chunks, and at most every `TELEMETRY_MAX_AGE`.
    pub fn stats(&self, session_id: &str, price_per_byte: HoprBalance) -> ipc::PixChunkStats {
        let session = self
            .sessions
            .lock()
            .expect("pix chunks lock poisoned")
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned();
        match session {
            Some(session) => derive(&session, &self.recent_telemetry(), price_per_byte),
            None => free(),
        }
    }

    fn recent_telemetry(&self) -> String {
        let mut cached = self.telemetry.lock().expect("pix telemetry lock poisoned");
        if let Some((at, text)) = cached.as_ref()
            && at.elapsed() < TELEMETRY_MAX_AGE
        {
            return text.clone();
        }
        let text = super::telemetry()
            .inspect_err(|err| tracing::warn!(%err, "no telemetry for PIX chunk stats"))
            .unwrap_or_default();
        *cached = Some((Instant::now(), text.clone()));
        text
    }
}

fn free() -> ipc::PixChunkStats {
    ipc::PixChunkStats {
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
    }
}

fn derive(session: &SessionChunks, telemetry: &str, price_per_byte: HoprBalance) -> ipc::PixChunkStats {
    let session_id = session.session_id.as_str();
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

    /// `PixChunks::stats` with a given scrape instead of the live registry.
    fn stats_with(chunks: &PixChunks, session_id: &str, telemetry: &str) -> ipc::PixChunkStats {
        let sessions = chunks.sessions.lock().expect("pix chunks lock poisoned");
        match sessions.iter().find(|s| s.session_id == session_id) {
            Some(session) => derive(session, telemetry, price()),
            None => free(),
        }
    }

    #[test]
    fn a_session_without_chunks_is_free() {
        let stats = PixChunks::default().stats("s1", price());
        assert_eq!(stats.phase, ipc::PixPhase::Free);
        assert_eq!(stats.bytes_bought, 0);
    }

    #[test]
    fn a_chunk_is_pending_until_its_deposit_is_sent() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 10.0, 4, 0));

        let pending = stats_with(&chunks, "s1", &telemetry(1_100, 10.0, 4, 0));
        assert_eq!(pending.phase, ipc::PixPhase::PaymentPending);

        let funded = stats_with(&chunks, "s1", &telemetry(1_100, 10.0, 5, 0));
        assert_eq!(funded.phase, ipc::PixPhase::Funded);
        assert_eq!(funded.chunks_paid, 1);
    }

    #[test]
    fn a_failed_payment_wins_over_paid_chunks() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));

        let stats = stats_with(&chunks, "s1", &telemetry(0, 0.0, 1, 1));
        assert_eq!(stats.phase, ipc::PixPhase::PaymentFailed);
        assert_eq!(stats.chunks_committed, 2);
        assert_eq!((stats.chunks_paid, stats.chunks_failed), (1, 1));
    }

    #[test]
    fn usage_counts_only_surbs_minted_after_the_first_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 0.0, 0, 0));

        let stats = stats_with(&chunks, "s1", &telemetry(1_100, 100.0, 1, 0));
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

        let stats = stats_with(&chunks, "s1", &telemetry(0, 0.0, 3, 0));
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
        assert_eq!(stats_with(&chunks, "old0", "").phase, ipc::PixPhase::Free);
        assert_ne!(
            stats_with(&chunks, &format!("old{MAX_SESSIONS}"), "").phase,
            ipc::PixPhase::Free
        );
    }
}
