pub use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use crate::{serde_utils, wire};

use std::collections::HashMap;
use std::fmt::{self, Display};
use std::time::{Duration, SystemTime};

pub use gnosis_vpn_ipc::FundingLevel;

/// Traffic/gas health plus wxHOPR/xDAI still needed to reach the ideal recommendation.
#[derive(Clone, Debug)]
pub struct FundingStatus {
    pub traffic: FundingLevel,
    pub gas: FundingLevel,
    /// wxHOPR still needed to reach the ideal recommendation; can be `None` even while `traffic` isn't `Good`.
    pub wxhopr_deficit: Option<Balance<WxHOPR>>,
    /// xDAI still needed to reach the ideal recommendation; `None` while `gas` is `Good`.
    pub xdai_deficit: Option<Balance<XDai>>,
    /// wxHOPR the Safe lacks for the next top-up pass over open channels; `None` when covered. Informational, never moves `traffic`.
    pub refill_shortfall: Option<Balance<WxHOPR>>,
}

// Retuned for the reconciled (pooled) quota counting; bounds inclusive.
const TRAFFIC_EMPTY_MAX_BYTES: u64 = 768 * 1024 * 1024;
const TRAFFIC_LOW_MAX_BYTES: u64 = 1536 * 1024 * 1024;
// 0.0015 / 0.0035 xDAI, in wei
const XDAI_EMPTY_BELOW_WEI: u64 = 1_500_000_000_000_000;
const XDAI_LOW_BELOW_WEI: u64 = 3_500_000_000_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingTool {
    NotStarted,
    InProgress,
    CompletedSuccess(#[serde(with = "serde_utils::system_time")] SystemTime),
    CompletedError(String),
}

impl FundingTool {
    /// Successful runs are only restartable once this much time has passed, so a
    /// misclick can't immediately re-trigger an on-chain funding transaction.
    const RERUN_COOLDOWN: Duration = Duration::from_secs(5 * 60);

    /// Time left before a successful run may be restarted, or `None` if it's not
    /// cooling down (still in progress, never run, errored, or cooldown elapsed).
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let FundingTool::CompletedSuccess(completed_at) = self else {
            return None;
        };
        let elapsed = completed_at.elapsed().unwrap_or_default();
        Self::RERUN_COOLDOWN
            .checked_sub(elapsed)
            .filter(|remaining| !remaining.is_zero())
    }
}

/// Data-throughput capacity for a wxHOPR stake at the current ticket price.
#[derive(Clone, Copy, Debug)]
pub struct Capacity {
    pub stake: Balance<WxHOPR>,
    pub expected_messages: u64,
    pub min_guaranteed_messages: u64,
    pub byte_capacity: u64,
}

impl Default for Capacity {
    fn default() -> Self {
        Capacity {
            stake: Balance::<WxHOPR>::zero(),
            expected_messages: 0,
            min_guaranteed_messages: 0,
            byte_capacity: 0,
        }
    }
}

/// Serde mirror of [`edgli::strategy::CapacityAllocations`], the funds paying for relays — open
/// outgoing channels, the unallocated Safe balance, and the node EOA (deposited funds not yet swept
/// into the Safe) — plus the funds paying for exit traffic.
#[derive(Clone, Debug, Default)]
pub struct CapacityAllocations {
    /// Open outgoing payment channels, keyed by destination peer.
    pub peer_allocations: HashMap<Address, Capacity>,
    /// wxHOPR on the node EOA, not yet swept into the Safe.
    pub node: Capacity,
    /// The unallocated wxHOPR balance held in the user's Safe contract.
    pub safe: Capacity,
    /// Pays for exit traffic; edgli doesn't report it, see [`ExitTrafficFunds::new`].
    pub exit_traffic: ExitTrafficFunds,
}

/// The funds paying for exit traffic: the Curvy float PIX deposits come out of.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExitTrafficFunds {
    /// Valued by [`CapacityReconciler`] at the rate paying for relays, so moving funds between the two keeps the total.
    pub capacity: Capacity,
    /// Data the funds buy from exits at this client's price per byte.
    pub bytes_from_exits: u64,
}

impl ExitTrafficFunds {
    /// Unvalued until [`CapacityReconciler::reconcile`] fills in the capacity.
    pub fn new(funds: Balance<WxHOPR>, price_per_byte: Balance<WxHOPR>) -> Self {
        let bytes_from_exits = if price_per_byte.is_zero() {
            0
        } else {
            u64::try_from(funds.amount() / price_per_byte.amount()).unwrap_or(u64::MAX)
        };
        Self {
            capacity: Capacity {
                stake: funds,
                ..Capacity::default()
            },
            bytes_from_exits,
        }
    }
}

/// Linear capacity of `stake` at the `base` stake's rate; `None` without a rate to scale from.
fn scale_capacity(
    stake: Balance<WxHOPR>,
    base: Balance<WxHOPR>,
    bytes: u64,
    msgs: u64,
    min_msgs: u64,
) -> Option<(u64, u64, u64)> {
    let tokens = |b: Balance<WxHOPR>| -> f64 {
        b.amount_in_base_units().parse().unwrap_or_else(|e| {
            tracing::warn!(balance = %b, error = %e, "failed to parse balance while scaling capacity");
            0.0
        })
    };
    let base = tokens(base);
    (base > 0.0).then(|| {
        let r = tokens(stake) / base;
        (
            (bytes as f64 * r) as u64,
            (msgs as f64 * r) as u64,
            (min_msgs as f64 * r) as u64,
        )
    })
}

impl From<edgli::strategy::Capacity> for Capacity {
    fn from(c: edgli::strategy::Capacity) -> Self {
        Capacity {
            stake: c.stake,
            expected_messages: c.expected_messages,
            min_guaranteed_messages: c.min_guaranteed_messages,
            byte_capacity: c.byte_capacity,
        }
    }
}

impl From<edgli::strategy::CapacityAllocations> for CapacityAllocations {
    fn from(a: edgli::strategy::CapacityAllocations) -> Self {
        CapacityAllocations {
            peer_allocations: a
                .peer_allocations
                .into_iter()
                .map(|(addr, c)| (addr, c.into()))
                .collect(),
            node: a.node.into(),
            safe: a.safe.into(),
            exit_traffic: ExitTrafficFunds::default(),
        }
    }
}

impl Display for CapacityAllocations {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "CapacityAllocations(node: {}, safe: {}, {} channels totalling {}, exit traffic: {})",
            self.node.stake,
            self.safe.stake,
            self.peer_allocations.len(),
            self.peer_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>(),
            self.exit_traffic.capacity.stake
        )
    }
}

/// Bounds how long an unexplained Safe/EOA drop is masked, so a real spend still surfaces.
const PENDING_ALLOCATION_MAX_POLLS: u8 = 5;

/// Stake presumed in flight (Safe/EOA → channel), folded into published capacity until indexed or expired.
#[derive(Clone, Copy, Debug)]
struct PendingAllocation {
    stake: Balance<WxHOPR>,
    expected_messages: u64,
    min_guaranteed_messages: u64,
    byte_capacity: u64,
    polls_left: u8,
}

/// Pooled totals of a raw snapshot, kept between polls to detect stake drops.
#[derive(Clone, Copy, Debug)]
struct SnapshotTotals {
    stake: Balance<WxHOPR>,
    channel_stake: Balance<WxHOPR>,
    exit_traffic_stake: Balance<WxHOPR>,
    expected_messages: u64,
    min_guaranteed_messages: u64,
    byte_capacity: u64,
}

impl SnapshotTotals {
    /// The funds paying for relays alone.
    fn relays(caps: &CapacityAllocations) -> Self {
        let channel_stake = caps.peer_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>();
        let sum = |f: fn(&Capacity) -> u64| {
            f(&caps.node) + f(&caps.safe) + caps.peer_allocations.values().map(f).sum::<u64>()
        };
        SnapshotTotals {
            stake: caps.node.stake + caps.safe.stake + channel_stake,
            channel_stake,
            exit_traffic_stake: Balance::zero(),
            expected_messages: sum(|c| c.expected_messages),
            min_guaranteed_messages: sum(|c| c.min_guaranteed_messages),
            byte_capacity: sum(|c| c.byte_capacity),
        }
    }

    /// Every allocation, the funds paying for exit traffic included.
    fn of(caps: &CapacityAllocations) -> Self {
        let relays = Self::relays(caps);
        let exit = &caps.exit_traffic.capacity;
        SnapshotTotals {
            stake: relays.stake + exit.stake,
            exit_traffic_stake: exit.stake,
            expected_messages: relays.expected_messages + exit.expected_messages,
            min_guaranteed_messages: relays.min_guaranteed_messages + exit.min_guaranteed_messages,
            byte_capacity: relays.byte_capacity + exit.byte_capacity,
            ..relays
        }
    }
}

/// Keeps the pooled capacity total conserved across the snapshot's non-atomic reads.
///
/// Channels are read from the indexer (lags the chain) before the live Safe/EOA
/// balances, so stake moving Safe→channel is counted nowhere for a poll or two.
/// The only legitimate *fast* Safe/EOA drop is a transfer toward a channel (usage
/// drains channel stakes and the exit-traffic funds instead, and a shield into the
/// exit-traffic funds is counted there), so an unmatched drop is presumed in-flight and
/// folded back into the published Safe capacity. The fold-in expires after
/// `PENDING_ALLOCATION_MAX_POLLS` polls so a real spend still surfaces, just late;
/// a transient over-count decays over the same window.
#[derive(Debug, Default)]
pub struct CapacityReconciler {
    prev: Option<SnapshotTotals>,
    pending: Option<PendingAllocation>,
}

impl CapacityReconciler {
    /// Snapshot to publish: in-flight stake folded into `safe`. Same shape as the
    /// raw snapshot, so downstream consumers summing components need no protocol change.
    pub fn reconcile(&mut self, mut raw: CapacityAllocations) -> CapacityAllocations {
        // Valued here so a snapshot with nothing left paying for relays still has the last rate.
        let exit_stake = raw.exit_traffic.capacity.stake;
        let (byte_capacity, expected_messages, min_guaranteed_messages) =
            self.scaled_capacity(exit_stake, &SnapshotTotals::relays(&raw));
        raw.exit_traffic.capacity = Capacity {
            stake: exit_stake,
            expected_messages,
            min_guaranteed_messages,
            byte_capacity,
        };
        let totals = SnapshotTotals::of(&raw);
        let pending_stake = match &self.prev {
            Some(prev) => {
                let carried = self.pending.map(|p| p.stake).unwrap_or_else(Balance::zero);
                // Channel drops (drainage, closure) and exit-traffic drops (paid exits) are real usage, not in-flight; `-` saturates at zero.
                let channel_drop = prev.channel_stake - totals.channel_stake;
                let exit_traffic_drop = prev.exit_traffic_stake - totals.exit_traffic_stake;
                prev.stake + carried - channel_drop - exit_traffic_drop - totals.stake
            }
            None => Balance::zero(),
        };

        self.pending = self.next_pending(pending_stake, &totals);
        self.prev = Some(totals);

        let mut published = raw;
        if let Some(p) = &self.pending {
            tracing::info!(stake = %p.stake, polls_left = p.polls_left, "counting in-flight stake toward published capacity");
            published.safe.stake += p.stake;
            published.safe.expected_messages += p.expected_messages;
            published.safe.min_guaranteed_messages += p.min_guaranteed_messages;
            published.safe.byte_capacity += p.byte_capacity;
        }
        published
    }

    fn next_pending(&self, stake: Balance<WxHOPR>, current: &SnapshotTotals) -> Option<PendingAllocation> {
        if stake.is_zero() {
            return None;
        }
        let polls_left = match &self.pending {
            // a further drop restarts the clock; otherwise keep counting down
            Some(old) if stake > old.stake => PENDING_ALLOCATION_MAX_POLLS,
            Some(old) => old.polls_left.saturating_sub(1),
            None => PENDING_ALLOCATION_MAX_POLLS,
        };
        if polls_left == 0 {
            tracing::warn!(%stake, "stake drop never resolved into a channel - accepting the lower total");
            return None;
        }
        let (byte_capacity, expected_messages, min_guaranteed_messages) = self.scaled_capacity(stake, current);
        Some(PendingAllocation {
            stake,
            expected_messages,
            min_guaranteed_messages,
            byte_capacity,
            polls_left,
        })
    }

    /// Scaled linearly from the freshest stake→capacity ratio on hand — the client
    /// holds neither ticket price nor win probability, so it cannot recompute
    /// capacities. A pending stake guarantees some fallback has non-zero stake.
    fn scaled_capacity(&self, stake: Balance<WxHOPR>, current: &SnapshotTotals) -> (u64, u64, u64) {
        let of_totals = |t: &SnapshotTotals| {
            scale_capacity(
                stake,
                t.stake,
                t.byte_capacity,
                t.expected_messages,
                t.min_guaranteed_messages,
            )
        };
        of_totals(current)
            .or_else(|| self.prev.as_ref().and_then(of_totals))
            .or_else(|| {
                self.pending.as_ref().and_then(|p| {
                    scale_capacity(
                        stake,
                        p.stake,
                        p.byte_capacity,
                        p.expected_messages,
                        p.min_guaranteed_messages,
                    )
                })
            })
            .unwrap_or((0, 0, 0))
    }
}

/// Recommended wxHOPR and xDAI balance to open the target number of channels.
/// `wxhopr` is the total to fund: channel stakes plus the one-time key-binding
/// (announcement) fee, which on some networks (e.g. rotsee: 0.01 wxHOPR) dwarfs the
/// channel stakes themselves. The breakdown fields mirror
/// `edgli::strategy::BalanceRecommendation`. Carries both the minimum
/// recommendation (computed once during onboarding, surfaced in the PreparingSafe
/// run mode and gating safe deployment) and the ideal recommendation (refreshed
/// periodically while running, feeding funding-issue checks and balance responses).
#[derive(Copy, Clone, Debug)]
pub struct BalanceRecommendation {
    /// Total wxHOPR to fund: channel stakes plus the fee to start.
    pub wxhopr: Balance<WxHOPR>,
    /// Recommended xDai balance for gas: the total amount to fund the node with.
    ///
    /// Not [`Self::xdai_fee_per_tx`], which is a ceiling on a single transaction rather than
    /// expected spend -- funding one transaction's worth would leave the node unable to finish
    /// starting up.
    pub xdai: Balance<XDai>,
    /// wxHOPR needed to stake the missing channels.
    pub channel_stakes: Balance<WxHOPR>,
    /// One-time key-binding fee still owed before the node can start;
    /// zero once the key is bound on-chain.
    pub fee_to_start: Balance<WxHOPR>,
    /// Number of on-chain transactions still needed before channel funding can
    /// begin (Safe deployment, Safe registration, key-binding announcement).
    pub txs_to_start: u64,
    /// Maximum xDAI fee per transaction (gas).
    pub xdai_fee_per_tx: Balance<XDai>,
    /// wxHOPR the next top-up pass over open channels will try to spend; not part of `wxhopr`.
    pub topup_headroom: Balance<WxHOPR>,
}

impl From<edgli::strategy::BalanceRecommendation> for BalanceRecommendation {
    fn from(rec: edgli::strategy::BalanceRecommendation) -> Self {
        BalanceRecommendation {
            wxhopr: rec.total_wxhopr(),
            xdai: rec.xdai_fund_amount,
            channel_stakes: rec.channel_stakes,
            fee_to_start: rec.fee_to_start,
            txs_to_start: rec.txs_to_start,
            xdai_fee_per_tx: rec.xdai_fee_per_tx,
            topup_headroom: rec.topup_headroom,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreSafe {
    pub node_xdai: Balance<XDai>,
    pub node_wxhopr: Balance<WxHOPR>,
}

impl Default for PreSafe {
    fn default() -> Self {
        Self {
            node_xdai: Balance::<XDai>::zero(),
            node_wxhopr: Balance::<WxHOPR>::zero(),
        }
    }
}

impl Display for PreSafe {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(node_xdai: {}, node_wxhopr: {})", self.node_xdai, self.node_wxhopr)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balances {
    pub node_xdai: Balance<XDai>,
    pub safe_wxhopr: Balance<WxHOPR>,
    pub channels_out: HashMap<Address, Balance<WxHOPR>>,
    /// None without a Curvy PIX pool, or when the pool could not be read.
    pub pix_float_wxhopr: Option<Balance<WxHOPR>>,
}

impl Display for Balances {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balances(node_xdai: {}, safe_wxhopr: {}, channels_out_wxhopr: {})",
            self.node_xdai,
            self.safe_wxhopr,
            self.channels_out.values().copied().sum::<Balance<WxHOPR>>()
        )
    }
}

fn traffic_level(bytes: u64) -> FundingLevel {
    if bytes <= TRAFFIC_EMPTY_MAX_BYTES {
        FundingLevel::Empty
    } else if bytes <= TRAFFIC_LOW_MAX_BYTES {
        FundingLevel::Low
    } else {
        FundingLevel::Good
    }
}

/// Pools every allocation location so funds unswept on the node EOA or paying for exit traffic still count.
pub fn to_funding_status(
    ideal: BalanceRecommendation,
    capacity_allocations: &CapacityAllocations,
    node_xdai: Balance<XDai>,
    safe_wxhopr: Balance<WxHOPR>,
) -> FundingStatus {
    let traffic = traffic_level(SnapshotTotals::of(capacity_allocations).byte_capacity);
    // Funds paying for exit traffic can't pay for relays, so the deficit follows the relay funds alone.
    let relays = SnapshotTotals::relays(capacity_allocations);

    // Safe funds missing channels first (wxhopr_deficit counts it there), only the rest can refill; `-` saturates.
    let safe_after_missing = safe_wxhopr - ideal.wxhopr;
    let refill_shortfall = Some(ideal.topup_headroom - safe_after_missing).filter(|d| !d.is_zero());

    let xdai_empty_below = Balance::<XDai>::from(XDAI_EMPTY_BELOW_WEI);
    let xdai_low_below = Balance::<XDai>::from(XDAI_LOW_BELOW_WEI);
    let gas = if node_xdai < xdai_empty_below {
        FundingLevel::Empty
    } else if node_xdai < xdai_low_below {
        FundingLevel::Low
    } else {
        FundingLevel::Good
    };

    // `-` on Balance saturates at zero; wxhopr_deficit is relative to `ideal` only, which ignores drained stake on already-open channels.
    let wxhopr_deficit = (traffic_level(relays.byte_capacity) != FundingLevel::Good)
        .then(|| ideal.wxhopr - relays.stake)
        .filter(|d| !d.is_zero());
    // Floored at xdai_low_below so this can't go None while gas isn't Good, even if `ideal` dips below it.
    let xdai_deficit = (gas != FundingLevel::Good)
        .then(|| ideal.xdai.max(xdai_low_below) - node_xdai)
        .filter(|d| !d.is_zero());

    FundingStatus {
        traffic,
        gas,
        wxhopr_deficit,
        xdai_deficit,
        refill_shortfall,
    }
}

impl From<&FundingStatus> for gnosis_vpn_ipc::FundingStatus {
    fn from(status: &FundingStatus) -> Self {
        Self {
            traffic: status.traffic,
            gas: status.gas,
            wxhopr_deficit: status.wxhopr_deficit.map(wire::wxhopr),
            xdai_deficit: status.xdai_deficit.map(wire::xdai),
            refill_shortfall: status.refill_shortfall.map(wire::wxhopr),
        }
    }
}

impl From<&Capacity> for gnosis_vpn_ipc::Capacity {
    fn from(capacity: &Capacity) -> Self {
        Self {
            stake: wire::wxhopr(capacity.stake),
            expected_messages: capacity.expected_messages,
            min_guaranteed_messages: capacity.min_guaranteed_messages,
            byte_capacity: capacity.byte_capacity,
        }
    }
}

impl From<&CapacityAllocations> for gnosis_vpn_ipc::CapacityAllocations {
    fn from(allocations: &CapacityAllocations) -> Self {
        Self {
            peer_allocations: allocations
                .peer_allocations
                .iter()
                .map(|(address, capacity)| (wire::address(address), capacity.into()))
                .collect(),
            node: (&allocations.node).into(),
            safe: (&allocations.safe).into(),
            exit_traffic: (&allocations.exit_traffic).into(),
        }
    }
}

impl From<&ExitTrafficFunds> for gnosis_vpn_ipc::ExitTrafficFunds {
    fn from(funds: &ExitTrafficFunds) -> Self {
        Self {
            stake: wire::wxhopr(funds.capacity.stake),
            byte_capacity: funds.capacity.byte_capacity,
            bytes_from_exits: funds.bytes_from_exits,
        }
    }
}

impl From<&BalanceRecommendation> for gnosis_vpn_ipc::BalanceRecommendation {
    fn from(rec: &BalanceRecommendation) -> Self {
        Self {
            wxhopr: wire::wxhopr(rec.wxhopr),
            xdai: wire::xdai(rec.xdai),
            channel_stakes: wire::wxhopr(rec.channel_stakes),
            fee_to_start: wire::wxhopr(rec.fee_to_start),
            txs_to_start: rec.txs_to_start,
            xdai_fee_per_tx: wire::xdai(rec.xdai_fee_per_tx),
            topup_headroom: wire::wxhopr(rec.topup_headroom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ideal(wxhopr: u64, xdai: u64) -> BalanceRecommendation {
        BalanceRecommendation {
            wxhopr: Balance::<WxHOPR>::from(wxhopr),
            xdai: Balance::<XDai>::from(xdai),
            channel_stakes: Balance::<WxHOPR>::from(wxhopr),
            fee_to_start: Balance::<WxHOPR>::zero(),
            txs_to_start: 0,
            xdai_fee_per_tx: Balance::<XDai>::from(xdai),
            topup_headroom: Balance::<WxHOPR>::zero(),
        }
    }

    fn capacity(stake: u64, bytes: u64) -> Capacity {
        Capacity {
            stake: Balance::<WxHOPR>::from(stake),
            expected_messages: 0,
            min_guaranteed_messages: 0,
            byte_capacity: bytes,
        }
    }

    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;

    /// Allocations with at most one open channel (to a fixed peer address).
    fn allocs(peer: Option<Capacity>, node: Capacity, safe: Capacity) -> CapacityAllocations {
        CapacityAllocations {
            peer_allocations: peer.map(|c| (Address::from([1u8; 20]), c)).into_iter().collect(),
            node,
            safe,
            exit_traffic: ExitTrafficFunds::default(),
        }
    }

    /// Exit-traffic funds as the reconciler publishes them: already valued.
    fn valued_exit_funds(stake: u64, bytes: u64) -> ExitTrafficFunds {
        ExitTrafficFunds {
            capacity: capacity(stake, bytes),
            bytes_from_exits: 0,
        }
    }

    #[test]
    fn traffic_empty_when_no_capacity_anywhere() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Empty);
    }

    #[test]
    fn traffic_pools_channel_safe_and_node_eoa_bytes() {
        // 320 MB each, pooled to 960 MB: above the 768 MB Empty bound, within Low.
        let allocations = allocs(
            Some(capacity(0, 320 * MB)),
            capacity(0, 320 * MB),
            capacity(0, 320 * MB),
        );
        let status = to_funding_status(
            ideal(0, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Low);
    }

    #[test]
    fn traffic_empty_up_to_768mb_inclusive() {
        let allocations = allocs(None, capacity(0, 768 * MB), Capacity::default());
        let status = to_funding_status(
            ideal(0, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Empty);
    }

    #[test]
    fn traffic_low_between_thresholds_up_to_1536mb_inclusive() {
        for bytes in [768 * MB + 1, 1536 * MB] {
            let allocations = allocs(None, capacity(0, bytes), Capacity::default());
            let status = to_funding_status(
                ideal(0, 0),
                &allocations,
                Balance::<XDai>::zero(),
                Balance::<WxHOPR>::zero(),
            );
            assert_eq!(status.traffic, FundingLevel::Low);
        }
    }

    #[test]
    fn traffic_good_above_1536mb() {
        // unswept EOA wxHOPR alone counts toward traffic.
        let allocations = allocs(None, capacity(0, 1536 * MB + 1), Capacity::default());
        let status = to_funding_status(
            ideal(0, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
    }

    #[test]
    fn gas_empty_below_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(1_000_000_000_000_000_u64), // 0.001 xDAI < 0.0015 threshold
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Empty);
    }

    #[test]
    fn gas_low_between_thresholds() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(2_000_000_000_000_000_u64), // 0.002 xDAI
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Low);
    }

    #[test]
    fn gas_good_at_or_above_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI, at the boundary
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Good);
    }

    #[test]
    fn balance_recommendation_from_edgli_totals_stakes_and_fee() {
        let rec = edgli::strategy::BalanceRecommendation {
            channel_stakes: Balance::<WxHOPR>::from(800u64),
            fee_to_start: Balance::<WxHOPR>::from(10_000u64),
            txs_to_start: 3,
            xdai_fee_per_tx: Balance::<XDai>::from(100u64),
            xdai_fund_amount: Balance::<XDai>::from(5_000u64),
            topup_headroom: Balance::<WxHOPR>::from(42u64),
        };
        let mirrored: BalanceRecommendation = rec.into();
        assert_eq!(mirrored.wxhopr, Balance::<WxHOPR>::from(10_800u64));
        assert_eq!(
            mirrored.topup_headroom,
            Balance::<WxHOPR>::from(42u64),
            "reported figures must survive the mirror, kept out of total wxhopr"
        );
        assert_eq!(
            mirrored.xdai,
            Balance::<XDai>::from(5_000u64),
            "xdai must mirror the fund amount, not one transaction's fee ceiling"
        );
        assert_eq!(mirrored.channel_stakes, Balance::<WxHOPR>::from(800u64));
        assert_eq!(mirrored.fee_to_start, Balance::<WxHOPR>::from(10_000u64));
        assert_eq!(mirrored.txs_to_start, 3);
        assert_eq!(mirrored.xdai_fee_per_tx, Balance::<XDai>::from(100u64));
    }

    #[test]
    fn wxhopr_deficit_none_when_traffic_good() {
        let allocations = allocs(None, capacity(1_000, 5 * GB), Capacity::default());
        let status = to_funding_status(
            ideal(100, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, None);
    }

    #[test]
    fn wxhopr_deficit_reported_when_traffic_not_good() {
        let allocations = allocs(None, capacity(30, 0), Capacity::default());
        let status = to_funding_status(
            ideal(100, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Empty);
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(70u64)));
    }

    #[test]
    fn xdai_deficit_none_when_gas_good() {
        let status = to_funding_status(
            ideal(0, 100),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Good);
        assert_eq!(status.xdai_deficit, None);
    }

    #[test]
    fn xdai_deficit_reported_when_gas_not_good() {
        let status = to_funding_status(
            ideal(0, 1_000_000_000_000_000_000_u64), // 1 xDAI ideal
            &CapacityAllocations::default(),
            Balance::<XDai>::from(1_000_000_000_000_000_u64), // 0.001 xDAI on hand
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Empty);
        assert_eq!(
            status.xdai_deficit,
            Some(Balance::<XDai>::from(999_000_000_000_000_000_u64))
        );
    }

    #[test]
    fn xdai_deficit_reported_even_when_ideal_is_below_low_threshold() {
        let node_xdai = Balance::<XDai>::from(2_000_000_000_000_000_u64); // 0.002 xDAI, in the Low band
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            node_xdai,
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.gas, FundingLevel::Low);
        assert_eq!(
            status.xdai_deficit,
            Some(Balance::<XDai>::from(3_500_000_000_000_000_u64) - node_xdai),
            "deficit must be floored at the Low threshold, not the (lower) ideal recommendation"
        );
    }

    #[test]
    fn traffic_counts_exit_traffic_funds() {
        // 320 MB paying for relays alone is Empty; 640 MB paying for exit traffic lifts the pool to Low.
        let allocations = CapacityAllocations {
            exit_traffic: valued_exit_funds(500, 640 * MB),
            ..allocs(None, Capacity::default(), capacity(100, 320 * MB))
        };
        let status = to_funding_status(
            ideal(1000, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(100u64),
        );
        assert_eq!(status.traffic, FundingLevel::Low);
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(900u64)));
    }

    #[test]
    fn exit_traffic_funds_never_hide_the_relay_deficit() {
        // Good overall thanks to 2 GB paying for exit traffic, yet the relays still lack 900.
        let allocations = CapacityAllocations {
            exit_traffic: valued_exit_funds(500, 2 * GB),
            ..allocs(None, Capacity::default(), capacity(100, 320 * MB))
        };
        let status = to_funding_status(
            ideal(1000, 0),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(100u64),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(900u64)));
    }

    #[test]
    fn good_traffic_and_gas_when_well_funded() {
        // 2 GB each on the channel, Safe, and node EOA = 6 GB pooled, above the 5 GB threshold.
        let allocations = allocs(Some(capacity(100, 2 * GB)), capacity(0, 2 * GB), capacity(100, 2 * GB));
        let status = to_funding_status(
            ideal(100, 100),
            &allocations,
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI — at the Good threshold
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.gas, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, None);
        assert_eq!(status.xdai_deficit, None);
    }

    // ---- CapacityReconciler ----
    // Fixtures keep byte_capacity = 10 × stake so linear scaling is easy to assert.

    /// stake in wei with bytes pinned at 10 × stake.
    fn cap10(stake: u64) -> Capacity {
        capacity(stake, stake * 10)
    }

    fn total_stake(caps: &CapacityAllocations) -> Balance<WxHOPR> {
        SnapshotTotals::of(caps).stake
    }

    fn total_bytes(caps: &CapacityAllocations) -> u64 {
        SnapshotTotals::of(caps).byte_capacity
    }

    #[test]
    fn reconcile_first_snapshot_passes_through() {
        let mut r = CapacityReconciler::default();
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_holds_total_while_channel_funding_is_unindexed() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // safe halved, no channel visible yet: the missing 100 is in flight
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(200u64));
        assert_eq!(total_bytes(&out), 2000);
        // channel indexed: raw is whole again, no fold-in remains
        let out = r.reconcile(allocs(Some(cap10(100)), Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn reconcile_recovered_safe_flake_does_not_double_count() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // safe lookup flaked to zero for one poll
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(0)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // flake recovered: fold-in must vanish, not stack on top
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_tracks_overlapping_fundings() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        // first 100 sent, unindexed
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // second 100 sent while the first arrives: raw total still 100
        let out = r.reconcile(allocs(Some(cap10(100)), Capacity::default(), cap10(0)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        // both indexed
        let second = (Address::from([2u8; 20]), cap10(100));
        let mut raw = allocs(Some(cap10(100)), Capacity::default(), cap10(0));
        raw.peer_allocations.insert(second.0, second.1);
        let out = r.reconcile(raw);
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::zero());
    }

    #[test]
    fn reconcile_channel_drainage_passes_through_immediately() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(Some(cap10(200)), Capacity::default(), Capacity::default()));
        // tickets spent: channel stake shrinks — that is real usage, not in-flight funds
        let out = r.reconcile(allocs(Some(cap10(150)), Capacity::default(), Capacity::default()));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(150u64));
    }

    #[test]
    fn reconcile_pending_expires_into_the_lower_total() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        let dropped = allocs(None, Capacity::default(), cap10(100));
        // the drop is masked while polls_left counts down from PENDING_ALLOCATION_MAX_POLLS...
        for _ in 0..PENDING_ALLOCATION_MAX_POLLS {
            let out = r.reconcile(dropped.clone());
            assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        }
        // ...then accepted as a real spend
        let out = r.reconcile(dropped);
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn reconcile_scales_folded_capacity_from_current_ratio() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(100)));
        // fixtures pin bytes at 10 × stake, so the folded-in 100 must carry 1000 bytes
        assert_eq!(out.safe.byte_capacity, 2000);
    }

    #[test]
    fn reconcile_never_dips_on_transient_overcount() {
        let mut r = CapacityReconciler::default();
        r.reconcile(allocs(None, cap10(100), cap10(100)));
        // EOA→Safe sweep landed between the two balance reads: briefly counted twice
        let out = r.reconcile(allocs(None, cap10(100), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(300u64));
        // correction back to 200 is masked until expiry (documented trade-off), never below 200
        let out = r.reconcile(allocs(None, Capacity::default(), cap10(200)));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(300u64));
    }

    /// Exit-traffic funds as the runner hands them over: unvalued, at 1 wei per byte.
    fn exit_funds(stake: u64) -> ExitTrafficFunds {
        ExitTrafficFunds::new(Balance::from(stake), Balance::from(1u64))
    }

    fn with_exit(exit: u64, safe: u64) -> CapacityAllocations {
        CapacityAllocations {
            exit_traffic: exit_funds(exit),
            ..allocs(None, Capacity::default(), cap10(safe))
        }
    }

    #[test]
    fn exit_traffic_funds_count_the_data_they_buy_from_exits() {
        let funds = ExitTrafficFunds::new(Balance::from(1000u64), Balance::from(10u64));
        assert_eq!(funds.bytes_from_exits, 100);
        assert_eq!(funds.capacity.byte_capacity, 0);
        let free = ExitTrafficFunds::new(Balance::from(1000u64), Balance::zero());
        assert_eq!(free.bytes_from_exits, 0);
    }

    #[test]
    fn reconcile_values_exit_traffic_at_the_relay_rate() {
        let mut r = CapacityReconciler::default();
        let out = r.reconcile(CapacityAllocations {
            exit_traffic: exit_funds(50),
            ..allocs(Some(cap10(100)), Capacity::default(), cap10(100))
        });
        assert_eq!(out.exit_traffic.capacity.stake, Balance::<WxHOPR>::from(50u64));
        assert_eq!(out.exit_traffic.capacity.byte_capacity, 500);
        assert_eq!(out.exit_traffic.bytes_from_exits, 50);
        assert_eq!(total_bytes(&out), 2500);
    }

    #[test]
    fn reconcile_without_any_rate_keeps_the_stake_but_no_bytes() {
        let out = CapacityReconciler::default().reconcile(with_exit(50, 0));
        assert_eq!(out.exit_traffic.capacity.stake, Balance::<WxHOPR>::from(50u64));
        assert_eq!(out.exit_traffic.capacity.byte_capacity, 0);
    }

    #[test]
    fn reconcile_shield_into_exit_traffic_keeps_the_total_without_a_fold_in() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_exit(0, 200));
        let out = r.reconcile(with_exit(100, 100));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(200u64));
        assert_eq!(total_bytes(&out), 2000);
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn reconcile_full_shield_keeps_the_last_relay_rate() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_exit(0, 200));
        // nothing left paying for relays to read a rate from
        let out = r.reconcile(with_exit(200, 0));
        assert_eq!(total_bytes(&out), 2000);
        let out = r.reconcile(with_exit(200, 0));
        assert_eq!(total_bytes(&out), 2000);
    }

    #[test]
    fn reconcile_paid_exits_pass_through_immediately() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_exit(100, 100));
        // a chunk paid to an exit is real usage, not in-flight funds
        let out = r.reconcile(with_exit(90, 100));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(190u64));
        assert_eq!(total_bytes(&out), 1900);
    }

    // 1 wxHOPR in wei, for the refill-shortfall fixtures below.
    const WXHOPR: u64 = 1_000_000_000_000_000_000;

    fn ideal_with_headroom(wxhopr: u64) -> BalanceRecommendation {
        BalanceRecommendation {
            topup_headroom: Balance::<WxHOPR>::from(wxhopr * WXHOPR),
            ..ideal(0, 0)
        }
    }

    #[test]
    fn refill_shortfall_none_when_the_safe_covers_the_topups() {
        let allocations = allocs(None, capacity(0, 2 * GB), Capacity::default());
        let status = to_funding_status(
            ideal_with_headroom(7),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(7 * WXHOPR),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.refill_shortfall, None);
    }

    #[test]
    fn refill_shortfall_reports_the_gap_without_lowering_traffic() {
        // The pool reads Good and stays Good even though the Safe holds 2 of the 7 wxHOPR the next fund pass wants.
        let allocations = allocs(None, capacity(0, 2 * GB), Capacity::default());
        let status = to_funding_status(
            ideal_with_headroom(7),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(2 * WXHOPR),
        );
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.refill_shortfall, Some(Balance::<WxHOPR>::from(5 * WXHOPR)));
    }

    #[test]
    fn refill_shortfall_counts_the_safe_once_across_missing_channels_and_refills() {
        // The Safe's 7 covers the refills or the missing channels alone, not both.
        let both = BalanceRecommendation {
            wxhopr: Balance::<WxHOPR>::from(4 * WXHOPR),
            ..ideal_with_headroom(7)
        };
        let allocations = allocs(None, capacity(0, 2 * GB), Capacity::default());
        let status = to_funding_status(
            both,
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(7 * WXHOPR),
        );
        assert_eq!(status.refill_shortfall, Some(Balance::<WxHOPR>::from(4 * WXHOPR)));
    }

    #[test]
    fn refill_shortfall_is_reported_alongside_an_empty_pool() {
        let status = to_funding_status(
            ideal_with_headroom(1),
            &CapacityAllocations::default(),
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert_eq!(status.traffic, FundingLevel::Empty);
        assert_eq!(status.refill_shortfall, Some(Balance::<WxHOPR>::from(WXHOPR)));
    }
}
