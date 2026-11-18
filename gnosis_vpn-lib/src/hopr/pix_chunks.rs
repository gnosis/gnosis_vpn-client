//! PIX chunks per session from hopr's events; usage is telemetry deltas since the first chunk, payments are attributed per chunk.

use edgli::hopr_lib::api::node::{HasExitIncentivization, PixEvent};
use edgli::hopr_lib::api::types::primitive::prelude::HoprBalance;
use edgli::hopr_lib::exports::transport::session::PIX_QUOTA_BYTES_PER_SHARE;
use futures_util::StreamExt;
use gnosis_vpn_ipc as ipc;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::ops::Bound::{Excluded, Included};
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
    /// (session, chunk) whose deposit outcome is not counted yet, oldest first.
    unsettled: VecDeque<(String, u32)>,
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
    outcomes: BTreeMap<u32, Deposit>,
}

impl SessionChunks {
    fn unpaid(&self) -> Option<Deposit> {
        // `committed` is the highest requested index, not the last event received.
        // An older failure may have been tolerated without ever starting a hold.
        let outcome = *self.outcomes.get(&self.committed)?;
        (self.confirmed < self.committed && outcome != Deposit::Paid).then_some(outcome)
    }

    fn has_refused(&self) -> bool {
        self.outcomes
            .range((Excluded(self.confirmed), Included(self.committed)))
            .any(|(_, outcome)| *outcome == Deposit::Refused)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Deposit {
    Paid,
    Failed,
    /// The strategy never retries it on its own, so the chunk stays unpaid.
    Deferred,
    /// The strategy refused to pay this chunk; paying again on a new session would be refused too.
    Refused,
}

impl fmt::Display for Deposit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Paid => "paid",
            Self::Failed => "failed",
            Self::Deferred => "deferred",
            Self::Refused => "refused",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Deposits {
    sent: u64,
    failed: u64,
    refused: u64,
    deferred: u64,
}

impl Deposits {
    fn read(telemetry: &str) -> Self {
        // a counter is missing until its first increment
        let count = |metric| unlabeled_u64(telemetry, metric).unwrap_or(0);
        Self {
            sent: count("hopr_strategy_pix_deposits_total"),
            failed: count("hopr_strategy_pix_deposits_failed_total"),
            refused: count("hopr_strategy_pix_deposits_rejected_total")
                + count("hopr_strategy_pix_deposits_over_budget_total"),
            deferred: count("hopr_strategy_pix_deposits_pending_total"),
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
        let entry = (session_id.to_string(), chunk_index);
        // Mirrors the strategy, which skips a chunk in flight or paid without counting anything.
        let is_unsettled = ledger.unsettled.contains(&entry);
        let is_paid = ledger
            .sessions
            .iter()
            .any(|s| s.session_id == session_id && s.outcomes.get(&chunk_index) == Some(&Deposit::Paid));
        if !is_unsettled && !is_paid {
            ledger.unsettled.push_back(entry);
        }
        if let Some(mut known) = take(&mut ledger.sessions, session_id) {
            if !is_unsettled && !is_paid {
                known.outcomes.remove(&chunk_index);
            }
            known.committed = known.committed.max(chunk_index);
            known.chunk_bytes = chunk_bytes;
            ledger.sessions.push_back(known);
            return;
        }
        if ledger.sessions.len() == MAX_SESSIONS
            && let Some(evicted) = ledger.sessions.pop_front()
        {
            ledger.unsettled.retain(|(id, _)| *id != evicted.session_id);
        }
        ledger.sessions.push_back(SessionChunks {
            session_id: session_id.to_string(),
            committed: chunk_index,
            confirmed: 0,
            chunk_bytes,
            surbs_before_first_chunk: gauge_u64(telemetry, SURBS_PRODUCED, session_id).unwrap_or(0),
            bytes_in_before_first_chunk: gauge_u64(telemetry, BYTES_IN, session_id).unwrap_or(0),
            outcomes: BTreeMap::new(),
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

    /// Latest unpaid outcome and any unconfirmed refusal, from one ledger snapshot.
    ///
    /// Batches can be prepared ahead of service. Do not interrupt a pending deposit just
    /// because another chunk failed: without the exit's front index, wait for all known
    /// unconfirmed requests to settle before allowing early recovery.
    pub fn payment_outcome(&self, session_id: &str) -> (Option<Deposit>, bool) {
        let ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        let Some(session) = ledger.sessions.iter().find(|s| s.session_id == session_id) else {
            return (None, false);
        };
        let pending = ledger
            .unsettled
            .iter()
            .any(|(id, index)| id == session_id && session.confirmed < *index && *index <= session.committed);
        let unpaid = if pending { None } else { session.unpaid() };
        // Keep this independent of the latest outcome: an older refusal still rules out recovery.
        (unpaid, session.has_refused())
    }

    /// The exit served the session again after a payment hold, so every chunk asked for so far is funded.
    pub fn confirm(&self, session_id: &str) {
        let mut ledger = self.ledger.lock().expect("pix chunks lock poisoned");
        if let Some(session) = ledger.sessions.iter_mut().find(|s| s.session_id == session_id) {
            session.confirmed = session.committed;
        }
    }

    /// Fresh every 4 s during a stall: settle deposits too, so a hold does not depend on status polling.
    pub fn surbs_consumed(&self, session_id: &str) -> Option<u64> {
        let telemetry = super::telemetry()
            .inspect_err(|err| tracing::warn!(%err, "no telemetry for PIX hold"))
            .ok()?;
        self.surbs_consumed_with(session_id, &telemetry)
    }

    fn surbs_consumed_with(&self, session_id: &str, telemetry: &str) -> Option<u64> {
        if !telemetry.is_empty() {
            self.ledger.lock().expect("pix chunks lock poisoned").settle(telemetry);
        }
        gauge_u64(telemetry, SURBS_CONSUMED, session_id)
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
    /// Oldest unsettled chunk first: node-wide counters force assuming request order, so an uncounted drop shifts later chunks.
    /// Mixed outcomes in one scrape are credited by category, not completion order. Until
    /// per-chunk strategy events replace this (#803), attribution can cause a spurious reconnect.
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
            refused: counted.refused.max(now.refused),
            deferred: counted.deferred.max(now.deferred),
        });
        let new_sent = now.sent.saturating_sub(counted.sent);
        let new_failed = now.failed.saturating_sub(counted.failed);
        let new_refused = now.refused.saturating_sub(counted.refused);
        let new_deferred = now.deferred.saturating_sub(counted.deferred);
        for _ in 0..new_sent {
            self.credit(Deposit::Paid);
        }
        for _ in 0..new_failed {
            self.credit(Deposit::Failed);
        }
        for _ in 0..new_refused {
            self.credit(Deposit::Refused);
        }
        for _ in 0..new_deferred {
            self.credit(Deposit::Deferred);
        }
    }

    fn credit(&mut self, outcome: Deposit) {
        let Some((session_id, chunk_index)) = self.unsettled.pop_front() else {
            return;
        };
        if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
            session.outcomes.insert(chunk_index, outcome);
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
    let count = |wanted: Deposit| session.outcomes.values().filter(|outcome| **outcome == wanted).count() as u64;
    let paid = count(Deposit::Paid);
    let failed = count(Deposit::Failed) + count(Deposit::Refused);
    // An early failure the exit got over must not mark the whole session failed.
    let phase = match session.outcomes.get(&session.committed) {
        Some(Deposit::Paid) => ipc::PixPhase::Funded,
        Some(Deposit::Failed | Deposit::Refused) => ipc::PixPhase::PaymentFailed,
        Some(Deposit::Deferred) | None => ipc::PixPhase::PaymentPending,
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
    fn reading_surbs_consumed_settles_new_deposit_outcomes() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        assert_eq!(chunks.payment_outcome("s1").0, None);

        let scrape = format!("{}{SURBS_CONSUMED}{{session_id=\"s1\"}} 42\n", telemetry(0, 0.0, 0, 1));
        assert_eq!(chunks.surbs_consumed_with("s1", &scrape), Some(42));
        assert_eq!(chunks.payment_outcome("s1").0, Some(Deposit::Failed));
    }

    #[test]
    fn a_refused_deposit_is_kept_apart_from_a_failed_one() {
        let chunks = PixChunks::default();
        for index in 1..=3 {
            chunks.record("s1", index, CHUNK, &telemetry(0, 0.0, 0, 0));
        }
        let scrape = format!(
            "{}hopr_strategy_pix_deposits_rejected_total 1\n\
             hopr_strategy_pix_deposits_over_budget_total 1\n",
            telemetry(0, 0.0, 0, 1)
        );
        let deposits = Deposits::read(&scrape);
        assert_eq!((deposits.failed, deposits.refused), (1, 2));
        let stats = stats_with(&chunks, "s1", &scrape);
        assert_eq!(stats.phase, ipc::PixPhase::PaymentFailed);
        assert_eq!(stats.chunks_failed, 3);
        assert_eq!(
            chunks.ledger.lock().unwrap().sessions[0].outcomes,
            BTreeMap::from([(1, Deposit::Failed), (2, Deposit::Refused), (3, Deposit::Refused)])
        );

        chunks.record("s2", 1, CHUNK, &scrape);
        stats_with(&chunks, "s2", &telemetry(0, 0.0, 0, 0));
        stats_with(&chunks, "s2", &scrape);
        assert_eq!(chunks.payment_outcome("s2").0, None);
    }

    #[test]
    fn an_unconfirmed_chunk_with_a_failed_deposit_is_unpaid() {
        for (metric, outcome) in [
            ("failed", Deposit::Failed),
            ("pending", Deposit::Deferred),
            ("rejected", Deposit::Refused),
            ("over_budget", Deposit::Refused),
        ] {
            let chunks = PixChunks::default();
            chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
            stats_with(&chunks, "s1", &format!("hopr_strategy_pix_deposits_{metric}_total 1\n"));
            assert_eq!(chunks.payment_outcome("s1").0, Some(outcome));
        }
    }

    #[test]
    fn an_unconfirmed_chunk_without_an_outcome_is_not_unpaid() {
        let chunks = PixChunks::default();
        assert_eq!(chunks.payment_outcome("s1").0, None);
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        assert_eq!(chunks.payment_outcome("s1").0, None);

        stats_with(&chunks, "s1", &telemetry(0, 0.0, 1, 0));
        assert_eq!(chunks.payment_outcome("s1").0, None);
        assert!(chunks.awaiting_payment("s1"));
    }

    #[test]
    fn an_old_failed_chunk_does_not_end_a_hold_for_a_pending_chunk() {
        use crate::connection::destination::{Address, Destination, DestinationSource, HopRouting, Meta};
        use crate::connection::up::{StallVerdict, Up};
        use std::time::SystemTime;

        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 1));
        // No hold was needed for chunk 1, so there has been no confirm().
        let mut conn = Up::new(Destination::new(
            "exit".to_string(),
            Address::from([7u8; 20]),
            HopRouting::try_from(1).unwrap(),
            Meta::default(),
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        ));
        let mut verdict = StallVerdict::Fine;
        for sample in 1..=5 {
            let stall = Duration::from_secs(sample * 4);
            verdict = conn.judge_stall(
                stall,
                chunks.awaiting_payment("s1"),
                Some(sample * 3),
                chunks.payment_outcome("s1").0,
                SystemTime::UNIX_EPOCH + stall,
            );
        }
        assert_eq!(verdict, StallVerdict::HoldStarted);
    }

    #[test]
    fn a_confirmed_chunk_with_a_failed_deposit_is_not_unpaid() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        stats_with(&chunks, "s1", &telemetry(0, 0.0, 0, 1));
        assert_eq!(chunks.payment_outcome("s1").0, Some(Deposit::Failed));
        chunks.confirm("s1");
        assert_eq!(chunks.payment_outcome("s1").0, None);
    }

    #[test]
    fn a_re_requested_chunk_is_pending_again() {
        for metric in ["failed", "pending", "rejected", "over_budget"] {
            let chunks = PixChunks::default();
            chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
            let scrape = format!("hopr_strategy_pix_deposits_{metric}_total 1\n");
            stats_with(&chunks, "s1", &scrape);
            assert!(chunks.payment_outcome("s1").0.is_some());
            chunks.record("s1", 1, CHUNK, &scrape);
            chunks.record("s1", 1, CHUNK, &scrape);
            assert_eq!(chunks.payment_outcome("s1").0, None);
            assert_eq!(stats_with(&chunks, "s1", &scrape).phase, ipc::PixPhase::PaymentPending);
            let ledger = chunks.ledger.lock().unwrap();
            assert!(ledger.sessions[0].outcomes.is_empty());
            assert_eq!(ledger.unsettled, VecDeque::from([("s1".to_string(), 1)]));
        }
    }

    #[test]
    fn the_highest_requested_chunk_controls_recovery_even_when_events_arrive_out_of_order() {
        let chunks = PixChunks::default();
        // Gaps are allowed too; only requests actually observed can be pending.
        for index in [7, 3, 1] {
            chunks.record("s1", index, CHUNK, &telemetry(0, 0.0, 0, 0));
        }
        let scrape = format!(
            "{}hopr_strategy_pix_deposits_rejected_total 1\n\
             hopr_strategy_pix_deposits_pending_total 1\n",
            telemetry(0, 0.0, 0, 1)
        );
        stats_with(&chunks, "s1", &scrape);
        assert_eq!(chunks.payment_outcome("s1"), (Some(Deposit::Failed), true));
    }

    #[test]
    fn an_older_failure_does_not_override_a_paid_latest_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 1));
        chunks.surbs_consumed_with("s1", &telemetry(0, 0.0, 1, 1));
        assert_eq!(chunks.payment_outcome("s1"), (None, false));
    }

    #[test]
    fn a_pending_lower_index_prevents_recovery_until_it_settles() {
        for metric in ["failed", "pending"] {
            let chunks = PixChunks::default();
            // A delayed lower-index request must not be interrupted by the newer failure.
            chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));
            let failed = format!("hopr_strategy_pix_deposits_{metric}_total 1\n");
            chunks.record("s1", 1, CHUNK, &failed);
            assert_eq!(chunks.payment_outcome("s1"), (None, false));
            chunks.surbs_consumed_with("s1", &format!("{failed}hopr_strategy_pix_deposits_total 1\n"));
            let reason = if metric == "failed" {
                Deposit::Failed
            } else {
                Deposit::Deferred
            };
            assert_eq!(chunks.payment_outcome("s1"), (Some(reason), false));
        }
    }

    #[test]
    fn pending_chunks_on_another_session_do_not_block_recovery() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s2", 1, CHUNK, &telemetry(0, 0.0, 0, 1));
        assert_eq!(chunks.payment_outcome("s1"), (Some(Deposit::Failed), false));
        assert_eq!(chunks.payment_outcome("s2"), (None, false));
        assert_eq!(chunks.payment_outcome("unknown"), (None, false));
    }

    #[test]
    fn a_snapshot_keeps_an_older_refusal_alongside_the_latest_failure() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        let refused = "hopr_strategy_pix_deposits_rejected_total 1\n";
        chunks.record("s1", 2, CHUNK, refused);
        assert_eq!(chunks.payment_outcome("s1"), (None, true));
        chunks.surbs_consumed_with("s1", &format!("{refused}hopr_strategy_pix_deposits_failed_total 1\n"));
        assert_eq!(chunks.payment_outcome("s1"), (Some(Deposit::Failed), true));
        chunks.confirm("s1");
        assert_eq!(chunks.payment_outcome("s1"), (None, false));
    }

    #[test]
    fn an_earlier_unpaid_chunk_does_not_hide_a_refused_deposit() {
        for metric in ["failed", "pending"] {
            let chunks = PixChunks::default();
            chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
            let first = format!("hopr_strategy_pix_deposits_{metric}_total 1\n");
            chunks.record("s1", 2, CHUNK, &first);
            let scrape = format!("{first}hopr_strategy_pix_deposits_rejected_total 1\n");
            chunks.surbs_consumed_with("s1", &scrape);

            assert_eq!(chunks.payment_outcome("s1"), (Some(Deposit::Refused), true));
            chunks.record("s2", 1, CHUNK, &scrape);
            assert!(!chunks.payment_outcome("s2").1);
            assert!(!chunks.payment_outcome("unknown").1);
        }
    }

    #[test]
    fn a_confirmed_refusal_does_not_restrict_a_later_failed_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        let refused = "hopr_strategy_pix_deposits_rejected_total 1\n";
        chunks.surbs_consumed_with("s1", refused);
        assert!(chunks.payment_outcome("s1").1);
        chunks.confirm("s1");
        assert!(!chunks.payment_outcome("s1").1);
        chunks.record("s1", 2, CHUNK, refused);
        chunks.surbs_consumed_with("s1", &format!("{refused}hopr_strategy_pix_deposits_failed_total 1\n"));
        assert_eq!(chunks.payment_outcome("s1").0, Some(Deposit::Failed));
        assert!(!chunks.payment_outcome("s1").1);
    }

    #[test]
    fn a_re_requested_refused_chunk_is_no_longer_refused() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        let refused = "hopr_strategy_pix_deposits_rejected_total 1\n";
        chunks.surbs_consumed_with("s1", refused);
        assert!(chunks.payment_outcome("s1").1);
        chunks.record("s1", 1, CHUNK, refused);
        assert!(!chunks.payment_outcome("s1").1);
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
    fn a_failed_latest_chunk_fails_the_session() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));

        let stats = stats_with(&chunks, "s1", &telemetry(0, 0.0, 1, 1));
        assert_eq!(stats.phase, ipc::PixPhase::PaymentFailed);
        assert_eq!(stats.chunks_committed, 2);
        assert_eq!((stats.chunks_paid, stats.chunks_failed), (1, 1));
    }

    #[test]
    fn phase_follows_the_latest_chunk() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 2, CHUNK, &telemetry(0, 0.0, 0, 0));

        stats_with(&chunks, "s1", &telemetry(0, 0.0, 0, 1));
        let stats = stats_with(&chunks, "s1", &telemetry(0, 0.0, 1, 1));
        assert_eq!(stats.phase, ipc::PixPhase::Funded);
        assert_eq!((stats.chunks_paid, stats.chunks_failed), (1, 1));
    }

    #[test]
    fn a_deferred_deposit_does_not_shift_later_chunks() {
        let chunks = PixChunks::default();
        let deferred = |n: u64| format!("hopr_strategy_pix_deposits_pending_total {n}\n");
        chunks.record("old", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("new", 1, CHUNK, &format!("{}{}", telemetry(0, 0.0, 0, 0), deferred(1)));

        let sent = format!("{}{}", telemetry(0, 0.0, 1, 0), deferred(1));
        assert_eq!(stats_with(&chunks, "new", &sent).phase, ipc::PixPhase::Funded);
        assert_eq!(stats_with(&chunks, "old", &sent).phase, ipc::PixPhase::PaymentPending);
    }

    #[test]
    fn a_repeated_event_for_a_chunk_is_counted_once() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s2", 1, CHUNK, &telemetry(0, 0.0, 0, 0));

        assert_eq!(
            stats_with(&chunks, "s1", &telemetry(0, 0.0, 1, 0)).phase,
            ipc::PixPhase::Funded
        );
        assert_eq!(
            stats_with(&chunks, "s2", &telemetry(0, 0.0, 2, 0)).phase,
            ipc::PixPhase::Funded
        );
    }

    #[test]
    fn a_repeated_event_for_a_paid_chunk_is_not_queued() {
        let chunks = PixChunks::default();
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 0, 0));
        chunks.record("s1", 1, CHUNK, &telemetry(0, 0.0, 1, 0));
        chunks.record("s2", 1, CHUNK, &telemetry(0, 0.0, 1, 0));

        let stats = stats_with(&chunks, "s2", &telemetry(0, 0.0, 2, 0));
        assert_eq!(stats.phase, ipc::PixPhase::Funded);
        assert_eq!(stats_with(&chunks, "s1", &telemetry(0, 0.0, 2, 0)).chunks_paid, 1);
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
