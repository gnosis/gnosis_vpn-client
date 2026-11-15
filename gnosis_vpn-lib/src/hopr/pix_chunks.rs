//! PIX chunks per session from hopr's events; usage is telemetry deltas since the first chunk, payments are attributed per chunk.

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

/// Only the live main session is ever looked up, which keeps it from being pushed out.
const MAX_SESSIONS: usize = 16;

/// Status is polled every few seconds and a scrape serializes the whole metrics registry.
const TELEMETRY_MAX_AGE: Duration = Duration::from_secs(5);

const SURBS_PRODUCED: &str = "hopr_session_surb_produced_total";
const BYTES_IN: &str = "hopr_session_transport_bytes_in_total";
const SURB_RATE: &str = "hopr_session_surb_rate_per_sec";
/// Counts every packet the exit got through to us on the session, keep-alives included.
const SURBS_CONSUMED: &str = "hopr_session_surb_consumed_total";

#[derive(Clone, Default)]
pub struct PixChunks {
    ledger: Arc<Mutex<Ledger>>,
    telemetry: Arc<Mutex<Option<(Instant, String)>>>,
}

#[derive(Debug, Default)]
struct Ledger {
    sessions: VecDeque<SessionChunks>,
    /// One entry per chunk whose deposit outcome is not counted yet, oldest first.
    unsettled: VecDeque<String>,
    /// Strategy counters as of the last settlement; `None` until the first chunk.
    counted: Option<Deposits>,
}

#[derive(Clone, Debug, PartialEq)]
struct SessionChunks {
    session_id: String,
    committed: u32,
    /// Highest chunk the exit was seen serving after; the exit never says so itself.
    confirmed: u32,
    chunk_bytes: u64,
    /// SURBs minted before the first chunk carried no share, so they are not spent quota.
    surbs_before_first_chunk: u64,
    bytes_in_before_first_chunk: u64,
    paid: u64,
    failed: u64,
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
        let mut ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        // Settled first, so deposits counted before this chunk can't be credited to it.
        ledger.settle(telemetry);
        ledger.unsettled.push_back(session_id.to_string());
        if let Some(mut known) = take(&mut ledger.sessions, session_id) {
            known.committed = known.committed.max(chunk_index);
            known.chunk_bytes = chunk_bytes;
            ledger.sessions.push_back(known);
            return;
        }
        if ledger.sessions.len() == MAX_SESSIONS
            && let Some(evicted) = ledger.sessions.pop_front()
        {
            ledger.unsettled.retain(|id| *id != evicted.session_id);
        }
        ledger.sessions.push_back(SessionChunks {
            session_id: session_id.to_string(),
            committed: chunk_index,
            confirmed: 0,
            chunk_bytes,
            surbs_before_first_chunk: gauge_u64(telemetry, SURBS_PRODUCED, session_id).unwrap_or(0),
            bytes_in_before_first_chunk: gauge_u64(telemetry, BYTES_IN, session_id).unwrap_or(0),
            paid: 0,
            failed: 0,
        });
    }

    /// Whether the exit asked this session for a chunk and hasn't been seen serving since.
    pub fn awaiting_payment(&self, session_id: &str) -> bool {
        let ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        ledger
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .is_some_and(|s| s.confirmed < s.committed)
    }

    /// The exit served the session again after a payment hold, so every chunk asked for so far is funded.
    pub fn confirm(&self, session_id: &str) {
        let mut ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        if let Some(session) = ledger.sessions.iter_mut().find(|s| s.session_id == session_id) {
            session.confirmed = session.committed;
        }
    }

    /// Fresh rather than cached: a hold is judged on whether this grew between two 4 s samples.
    pub fn surbs_consumed(&self, session_id: &str) -> Option<u64> {
        let telemetry = super::telemetry()
            .inspect_err(|err| tracing::warn!(%err, "no telemetry for PIX hold"))
            .ok()?;
        gauge_u64(&telemetry, SURBS_CONSUMED, session_id)
    }

    /// Reads telemetry only once the session has chunks, and at most every `TELEMETRY_MAX_AGE`.
    pub fn stats(&self, session_id: &str, price_per_byte: HoprBalance) -> ipc::PixChunkStats {
        let has_chunks = {
            let ledger = self.ledger.lock().expect("pix chunks lock poisoned");
            ledger.sessions.iter().any(|s| s.session_id == session_id)
        };
        if !has_chunks {
            return free();
        }
        let telemetry = self.recent_telemetry();
        let mut ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        ledger.settle(&telemetry);
        match take(&mut ledger.sessions, session_id) {
            Some(session) => {
                let stats = derive(&session, &telemetry, price_per_byte);
                ledger.sessions.push_back(session);
                stats
            }
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

impl Ledger {
    /// Credits new deposits to the oldest unsettled chunks; node-wide counters force assuming request order.
    fn settle(&mut self, telemetry: &str) {
        // a failed scrape reads as all-zero counters, which would look like a reset
        if telemetry.is_empty() {
            return;
        }
        let now = Deposits::read(telemetry);
        let Some(counted) = self.counted else {
            self.counted = Some(now);
            return;
        };
        // A cached scrape can be older than the one last settled.
        self.counted = Some(Deposits {
            sent: counted.sent.max(now.sent),
            failed: counted.failed.max(now.failed),
        });
        let new_sent = now.sent.saturating_sub(counted.sent);
        let new_failed = now.failed.saturating_sub(counted.failed);
        for _ in 0..new_sent {
            self.credit(|session| session.paid += 1);
        }
        for _ in 0..new_failed {
            self.credit(|session| session.failed += 1);
        }
    }

    fn credit(&mut self, outcome: impl FnOnce(&mut SessionChunks)) {
        let Some(session_id) = self.unsettled.pop_front() else {
            return;
        };
        if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
            outcome(session);
        }
    }
}

/// Removes the session so the caller can push it back as the most recently used.
fn take(sessions: &mut VecDeque<SessionChunks>, session_id: &str) -> Option<SessionChunks> {
    let index = sessions.iter().position(|s| s.session_id == session_id)?;
    sessions.remove(index)
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
        bytes_spent: None,
        bytes_left: None,
        bytes_received: None,
        empty_in: None,
    }
}

fn derive(session: &SessionChunks, telemetry: &str, price_per_byte: HoprBalance) -> ipc::PixChunkStats {
    let session_id = session.session_id.as_str();
    let committed = u64::from(session.committed);
    let paid = session.paid.min(committed);
    let failed = session.failed;
    let phase = if failed > 0 {
        ipc::PixPhase::PaymentFailed
    } else if paid < committed {
        ipc::PixPhase::PaymentPending
    } else {
        ipc::PixPhase::Funded
    };

    let bytes_bought = committed * session.chunk_bytes;
    let bytes_spent = gauge_u64(telemetry, SURBS_PRODUCED, session_id)
        .map(|produced| produced.saturating_sub(session.surbs_before_first_chunk) * PIX_QUOTA_BYTES_PER_SHARE);
    let bytes_left = bytes_spent.map(|spent| bytes_bought.saturating_sub(spent));
    let bytes_received = gauge_u64(telemetry, BYTES_IN, session_id)
        .map(|bytes_in| bytes_in.saturating_sub(session.bytes_in_before_first_chunk));
    let bytes_per_sec = gauge_value(telemetry, SURB_RATE, session_id)
        .map(|surbs| surbs * PIX_QUOTA_BYTES_PER_SHARE as f64)
        .filter(|rate| *rate > 0.0);
    let empty_in = match (bytes_left, bytes_per_sec) {
        // a near-zero rate overflows Duration; that is "not draining", not a panic
        (Some(left), Some(rate)) => Duration::try_from_secs_f64(left as f64 / rate).ok(),
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
        bytes_spent,
        bytes_left,
        bytes_received,
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
        let mut ledger = chunks.ledger.lock().expect("pix chunks lock poisoned");
        ledger.settle(telemetry);
        match ledger.sessions.iter().find(|s| s.session_id == session_id) {
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
    fn an_old_sessions_late_deposit_is_not_credited_to_the_new_session() {
        let chunks = PixChunks::default();
        chunks.record("old", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("new", 1, CHUNK, &telemetry(0, 0.0, 0, 0));

        let late = telemetry(0, 0.0, 1, 0);
        assert_eq!(stats_with(&chunks, "new", &late).phase, ipc::PixPhase::PaymentPending);
        assert_eq!(stats_with(&chunks, "old", &late).phase, ipc::PixPhase::Funded);
    }

    #[test]
    fn an_old_sessions_failed_deposit_does_not_fail_the_new_session() {
        let chunks = PixChunks::default();
        chunks.record("old", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("new", 1, CHUNK, &telemetry(0, 0.0, 0, 0));

        let failed = telemetry(0, 0.0, 0, 1);
        assert_eq!(stats_with(&chunks, "old", &failed).phase, ipc::PixPhase::PaymentFailed);
        assert_eq!(stats_with(&chunks, "new", &failed).phase, ipc::PixPhase::PaymentPending);
    }

    #[test]
    fn an_older_cached_scrape_does_not_recount_a_deposit() {
        let chunks = PixChunks::default();
        chunks.record("old", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("new", 1, CHUNK, &telemetry(0, 0.0, 1, 0));

        stats_with(&chunks, "new", &telemetry(0, 0.0, 0, 0));
        let stats = stats_with(&chunks, "new", &telemetry(0, 0.0, 1, 0));
        assert_eq!(stats.chunks_paid, 0);
    }

    #[test]
    fn deposits_counted_before_a_chunk_are_not_credited_to_it() {
        let chunks = PixChunks::default();
        chunks.record("old", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("new", 1, CHUNK, &telemetry(0, 0.0, 1, 0));

        let stats = stats_with(&chunks, "new", &telemetry(0, 0.0, 1, 0));
        assert_eq!(stats.chunks_paid, 0);
    }

    #[test]
    fn a_chunk_awaits_payment_until_the_exit_is_seen_serving() {
        let chunks = PixChunks::default();
        assert!(!chunks.awaiting_payment("s1"));
        chunks.record("s1", 1, CHUNK, "");
        assert!(chunks.awaiting_payment("s1"));
        chunks.confirm("s1");
        assert!(!chunks.awaiting_payment("s1"));
        chunks.record("s1", 2, CHUNK, "");
        assert!(chunks.awaiting_payment("s1"));
    }

    #[test]
    fn spending_counts_only_surbs_minted_after_the_first_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 0.0, 0, 0));

        let stats = stats_with(&chunks, "s1", &telemetry(1_100, 100.0, 1, 0));
        let spent = 100 * PIX_QUOTA_BYTES_PER_SHARE;
        assert_eq!(stats.bytes_spent, Some(spent));
        assert_eq!(stats.bytes_left, Some(CHUNK - spent));
        let secs = (CHUNK - spent) as f64 / (100.0 * PIX_QUOTA_BYTES_PER_SHARE as f64);
        assert_eq!(stats.empty_in, Some(Duration::from_secs_f64(secs)));
    }

    #[test]
    fn a_near_zero_rate_leaves_empty_in_unknown() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(1_000, 0.0, 0, 0));

        // hopr's delta-summed gauge reads ~1e-14 instead of 0 once production stops (GNO-1216)
        let stats = stats_with(&chunks, "s1", &telemetry(1_100, 1.42e-14, 1, 0));
        assert_eq!(stats.empty_in, None);
    }

    #[test]
    fn received_counts_bytes_since_the_first_chunk_apart_from_spending() {
        let chunks = PixChunks::default();
        let bytes_in = |n: u64| format!("{BYTES_IN}{{session_id=\"s1\"}} {n}\n");
        chunks.record(
            "s1",
            1,
            CHUNK,
            &format!("{}{}", telemetry(1_000, 0.0, 0, 0), bytes_in(40)),
        );

        let stats = stats_with(
            &chunks,
            "s1",
            &format!("{}{}", telemetry(5_000, 0.0, 1, 0), bytes_in(50)),
        );
        assert_eq!(stats.bytes_spent, Some(4_000 * PIX_QUOTA_BYTES_PER_SHARE));
        assert_eq!(stats.bytes_received, Some(10));
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

    #[test]
    fn a_polled_session_is_not_pushed_out() {
        let chunks = PixChunks::default();
        chunks.record("main", 1, CHUNK, "");
        for i in 0..MAX_SESSIONS {
            chunks.record(&format!("bridge{i}"), 1, CHUNK, "");
            chunks.stats("main", price());
        }
        assert_ne!(stats_with(&chunks, "main", "").phase, ipc::PixPhase::Free);
    }
}
