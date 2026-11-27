pub use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use crate::wire;

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display};
use std::time::{Duration, Instant};

pub use gnosis_vpn_ipc::FundingLevel;

/// Traffic/gas health plus wxHOPR/xDAI still needed to reach the ideal recommendation.
#[derive(Clone, Debug)]
pub struct FundingStatus {
    pub traffic: FundingLevel,
    pub gas: FundingLevel,
    /// wxHOPR still needed for `DATA_TARGET_BYTES` at the PIX price, at least back to Good; `None` while `traffic` is `Good`.
    pub wxhopr_deficit: Option<Balance<WxHOPR>>,
    /// xDAI still needed to reach the ideal recommendation; `None` while `gas` is `Good`.
    pub xdai_deficit: Option<Balance<XDai>>,
    /// wxHOPR the Safe lacks for the next top-up pass over open channels; `None` when covered. Informational, never moves `traffic`.
    pub refill_shortfall: Option<Balance<WxHOPR>>,
}

// Channels edgli keeps open when it closes the rest for a PIX pool refill.
const RECLAIM_KEEPS_CHANNELS: u64 = 1;
// Low up to this many times the funds that are Empty.
const LOW_FACTOR: u64 = 2;
// Data the wxHOPR deficit tops the account up to, at the PIX price.
const DATA_TARGET_BYTES: u64 = 30 * 1024 * 1024 * 1024;
// 0.0015 / 0.0035 xDAI, in wei
const XDAI_EMPTY_BELOW_WEI: u64 = 1_500_000_000_000_000;
const XDAI_LOW_BELOW_WEI: u64 = 3_500_000_000_000_000;

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
/// into the Safe) — plus the channels pending closure and the funds paying for exit traffic.
#[derive(Clone, Debug, Default)]
pub struct CapacityAllocations {
    /// Open outgoing payment channels, keyed by destination peer.
    pub peer_allocations: HashMap<Address, Capacity>,
    /// Outgoing channels pending closure; their stake can't pay for relays but returns to the Safe once closed.
    pub closing: HashMap<Address, Capacity>,
    /// wxHOPR on the node EOA, not yet swept into the Safe.
    pub node: Capacity,
    /// The unallocated wxHOPR balance held in the user's Safe contract.
    pub safe: Capacity,
    /// Pays for exit traffic; edgli doesn't report it, see [`ExitTrafficFunds::new`].
    pub exit_traffic: ExitTrafficFunds,
}

/// The funds paying for exit traffic: the Curvy PIX pool deposits come out of.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExitTrafficFunds {
    /// Valued by [`CapacityReconciler`] at the rate paying for relays, so moving funds between the two keeps the total.
    pub capacity: Capacity,
    /// Data the funds buy from exits at this client's price per byte.
    pub bytes_from_exits: u64,
    /// This client's PIX price per byte; zero without a PIX pool.
    pub price_per_byte: Balance<WxHOPR>,
}

impl ExitTrafficFunds {
    /// Unvalued until [`CapacityReconciler::reconcile`] fills in the capacity.
    pub fn new(funds: Balance<WxHOPR>, price_per_byte: Balance<WxHOPR>) -> Self {
        Self {
            capacity: Capacity {
                stake: funds,
                ..Capacity::default()
            },
            bytes_from_exits: edgli::strategy::bytes_at_pix_price(funds, price_per_byte),
            price_per_byte,
        }
    }
}

impl CapacityAllocations {
    /// All wxHOPR, channel stakes included, valued at the PIX price.
    pub fn total_bytes_at_pix_price(&self) -> u64 {
        edgli::strategy::bytes_at_pix_price(SnapshotTotals::of(self).stake, self.exit_traffic.price_per_byte)
    }

    /// Every outgoing channel counted, open or closing.
    pub fn channels(&self) -> HashSet<Address> {
        self.peer_allocations
            .keys()
            .chain(self.closing.keys())
            .copied()
            .collect()
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
            closing: HashMap::new(),
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
            "CapacityAllocations(node: {}, safe: {}, {} channels totalling {}, {} closing totalling {}, exit traffic: {})",
            self.node.stake,
            self.safe.stake,
            self.peer_allocations.len(),
            self.peer_allocations.values().map(|c| c.stake).sum::<Balance<WxHOPR>>(),
            self.closing.len(),
            self.closing.values().map(|c| c.stake).sum::<Balance<WxHOPR>>(),
            self.exit_traffic.capacity.stake
        )
    }
}

/// Equal channel reads in a row after which a node start's channel list counts as loaded.
const CHANNEL_SETTLE_READS: u8 = 3;
/// Trust the channel list this long after the node started, even if it keeps changing or can't be read.
const CHANNEL_SETTLE_MAX_WAIT: Duration = Duration::from_secs(60);

/// A started node fills its channel list from the indexer after it reports ready; balances wait until the list stops changing.
#[derive(Debug)]
pub struct ChannelSettle {
    since: Instant,
    last: Option<HashSet<Address>>,
    equal_reads: u8,
}

impl ChannelSettle {
    pub fn new(since: Instant) -> Self {
        Self {
            since,
            last: None,
            equal_reads: 0,
        }
    }

    /// Records one read of the channel list; once settled, later changes don't unsettle it.
    pub fn observe(&mut self, channels: HashSet<Address>) {
        if self.equal_reads >= CHANNEL_SETTLE_READS {
            return;
        }
        self.equal_reads = if self.last.as_ref() == Some(&channels) {
            self.equal_reads + 1
        } else {
            1
        };
        self.last = Some(channels);
    }

    pub fn is_settled(&self, now: Instant) -> bool {
        let list_stable = self.equal_reads >= CHANNEL_SETTLE_READS;
        let waited_long_enough = now.duration_since(self.since) >= CHANNEL_SETTLE_MAX_WAIT;
        list_stable || waited_long_enough
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
            exit_traffic_stake: Balance::zero(),
            expected_messages: sum(|c| c.expected_messages),
            min_guaranteed_messages: sum(|c| c.min_guaranteed_messages),
            byte_capacity: sum(|c| c.byte_capacity),
        }
    }

    /// Every allocation: closing channels and the funds paying for exit traffic included.
    fn of(caps: &CapacityAllocations) -> Self {
        let relays = Self::relays(caps);
        let exit = &caps.exit_traffic.capacity;
        let closing_stake = caps.closing.values().map(|c| c.stake).sum::<Balance<WxHOPR>>();
        let closing = |f: fn(&Capacity) -> u64| caps.closing.values().map(f).sum::<u64>();
        SnapshotTotals {
            stake: relays.stake + closing_stake + exit.stake,
            exit_traffic_stake: exit.stake,
            expected_messages: relays.expected_messages + closing(|c| c.expected_messages) + exit.expected_messages,
            min_guaranteed_messages: relays.min_guaranteed_messages
                + closing(|c| c.min_guaranteed_messages)
                + exit.min_guaranteed_messages,
            byte_capacity: relays.byte_capacity + closing(|c| c.byte_capacity) + exit.byte_capacity,
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
    /// Channel stakes of the previous snapshot, open or closing, to tell drainage from a channel done closing.
    prev_channels: HashMap<Address, Balance<WxHOPR>>,
    pending: Option<PendingAllocation>,
}

impl CapacityReconciler {
    /// Snapshot to publish: in-flight stake folded into `safe`. Same shape as the
    /// raw snapshot, so downstream consumers summing components need no protocol change.
    pub fn reconcile(&mut self, mut raw: CapacityAllocations) -> CapacityAllocations {
        // Valued here so a snapshot with nothing left paying for relays still has the last rate.
        let relays = SnapshotTotals::relays(&raw);
        raw.exit_traffic.capacity = self.valued(raw.exit_traffic.capacity.stake, &relays);
        for channel in raw.closing.values_mut() {
            *channel = self.valued(channel.stake, &relays);
        }
        let totals = SnapshotTotals::of(&raw);
        let pending_stake = match &self.prev {
            Some(prev) => {
                let carried = self.pending.map(|p| p.stake).unwrap_or_else(Balance::zero);
                // Channel drainage and exit-traffic drops (paid exits) are real usage, not in-flight; `-` saturates at zero.
                let channel_drop = self.channel_drainage(&raw);
                let exit_traffic_drop = prev.exit_traffic_stake - totals.exit_traffic_stake;
                prev.stake + carried - channel_drop - exit_traffic_drop - totals.stake
            }
            None => Balance::zero(),
        };

        self.pending = self.next_pending(pending_stake, &totals);
        self.prev = Some(totals);
        self.prev_channels = raw
            .peer_allocations
            .iter()
            .chain(&raw.closing)
            .map(|(address, c)| (*address, c.stake))
            .collect();

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

    /// Only drops apply: a top-up reaches the pool before the Safe read shows it leaving.
    pub fn with_pix_pool(&self, published: &CapacityAllocations, funds: Balance<WxHOPR>) -> CapacityAllocations {
        let mut fresher = published.clone();
        if funds < published.exit_traffic.capacity.stake {
            fresher.exit_traffic = ExitTrafficFunds::new(funds, published.exit_traffic.price_per_byte);
            fresher.exit_traffic.capacity = self.valued(funds, &SnapshotTotals::relays(published));
        }
        fresher
    }

    /// Drainage only: a channel moving to closing or gone closed is folded in until its stake reaches the Safe.
    fn channel_drainage(&self, raw: &CapacityAllocations) -> Balance<WxHOPR> {
        raw.peer_allocations
            .iter()
            .chain(&raw.closing)
            .filter_map(|(address, now)| self.prev_channels.get(address).map(|prev| *prev - now.stake))
            .sum()
    }

    /// `stake` at the rate paying for relays.
    fn valued(&self, stake: Balance<WxHOPR>, relays: &SnapshotTotals) -> Capacity {
        let (byte_capacity, expected_messages, min_guaranteed_messages) = self.scaled_capacity(stake, relays);
        Capacity {
            stake,
            expected_messages,
            min_guaranteed_messages,
            byte_capacity,
        }
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
    /// Total wxHOPR to fund: channel stakes, the PIX pool and the fee to start.
    pub wxhopr: Balance<WxHOPR>,
    /// Recommended xDai balance for gas: the total amount to fund the node with.
    ///
    /// Not [`Self::xdai_fee_per_tx`], which is a ceiling on a single transaction rather than
    /// expected spend -- funding one transaction's worth would leave the node unable to finish
    /// starting up.
    pub xdai: Balance<XDai>,
    /// wxHOPR needed to stake the missing channels.
    pub channel_stakes: Balance<WxHOPR>,
    /// PIX pool funding shielded from the Safe once; only onboarding counts it.
    pub pix_pool: Balance<WxHOPR>,
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
    /// wxHOPR one PIX pool refill shields from the Safe; zero without a pool.
    pub pix_top_up: Balance<WxHOPR>,
    /// PIX pool balance below which a refill is shielded; zero without a pool.
    pub pix_min_available: Balance<WxHOPR>,
    /// The most wxHOPR the strategy leaves in one channel; zero leaves the funding levels uncountable.
    pub channel_max_balance: Balance<WxHOPR>,
}

impl From<edgli::strategy::BalanceRecommendation> for BalanceRecommendation {
    fn from(rec: edgli::strategy::BalanceRecommendation) -> Self {
        BalanceRecommendation {
            wxhopr: rec.total_wxhopr(),
            xdai: rec.xdai_fund_amount,
            channel_stakes: rec.channel_stakes,
            pix_pool: rec.pix_float,
            fee_to_start: rec.fee_to_start,
            txs_to_start: rec.txs_to_start,
            xdai_fee_per_tx: rec.xdai_fee_per_tx,
            topup_headroom: rec.topup_headroom,
            pix_top_up: Balance::zero(),
            pix_min_available: Balance::zero(),
            channel_max_balance: Balance::zero(),
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
    pub pix_pool_wxhopr: Option<Balance<WxHOPR>>,
    /// Safe wxHOPR the next PIX pool top-up keeps from the channels; None without a Curvy PIX pool.
    pub pix_reserved: Option<Balance<WxHOPR>>,
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

/// The channel sizes are unknown, so the unusable funds can't be counted.
#[derive(Debug, thiserror::Error)]
#[error("unknown channel size")]
pub struct UnknownChannelSize;

/// A channel is topped up once it is down to the threshold, never above that plus one top-up.
pub fn channel_max_balance(funding: &hopr_strategy::channel_lifecycle::ResolvedFunding) -> Balance<WxHOPR> {
    funding
        .initial_balance
        .max(funding.lower_balance_threshold + funding.topup_balance)
}

/// wxHOPR left unusable once the Safe can't pay a PIX pool refill: the channels kept open and less than one refill.
fn unusable_funds(ideal: &BalanceRecommendation) -> Result<Balance<WxHOPR>, UnknownChannelSize> {
    if ideal.channel_max_balance.is_zero() {
        return Err(UnknownChannelSize);
    }
    // `pix-test` has no PIX pool to refill.
    Ok(ideal.channel_max_balance * RECLAIM_KEEPS_CHANNELS + ideal.pix_top_up)
}

/// Traffic follows all wxHOPR against the funds that end up unusable; there is no level without the channel sizes.
pub fn to_funding_status(
    ideal: BalanceRecommendation,
    capacity_allocations: &CapacityAllocations,
    node_xdai: Balance<XDai>,
    safe_wxhopr: Balance<WxHOPR>,
    pix_reserved: Balance<WxHOPR>,
) -> Result<FundingStatus, UnknownChannelSize> {
    let empty_max = unusable_funds(&ideal)?;
    let low_max = empty_max * LOW_FACTOR;
    let total = SnapshotTotals::of(capacity_allocations).stake;
    let traffic = if total <= empty_max {
        FundingLevel::Empty
    } else if total <= low_max {
        FundingLevel::Low
    } else {
        FundingLevel::Good
    };

    // The PIX reserve is not for channels; Safe funds missing channels first, only the rest can refill; `-` saturates.
    let safe_after_missing = safe_wxhopr - pix_reserved - ideal.wxhopr;
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

    // No target without a PIX price (`pix-test`), so just past Low; `-` saturates.
    let data_target = capacity_allocations.exit_traffic.price_per_byte * DATA_TARGET_BYTES;
    let wxhopr_deficit = (traffic != FundingLevel::Good)
        .then(|| (data_target - total).max(low_max - total + Balance::<WxHOPR>::from(1u64)));
    // Floored at xdai_low_below so this can't go None while gas isn't Good, even if `ideal` dips below it.
    let xdai_deficit = (gas != FundingLevel::Good)
        .then(|| ideal.xdai.max(xdai_low_below) - node_xdai)
        .filter(|d| !d.is_zero());

    Ok(FundingStatus {
        traffic,
        gas,
        wxhopr_deficit,
        xdai_deficit,
        refill_shortfall,
    })
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
            // Closing channels still hold the user's stake, so clients count them with the open ones.
            peer_allocations: allocations
                .peer_allocations
                .iter()
                .chain(&allocations.closing)
                .map(|(address, capacity)| (wire::address(address), capacity.into()))
                .collect(),
            node: (&allocations.node).into(),
            safe: (&allocations.safe).into(),
            exit_traffic: gnosis_vpn_ipc::ExitTrafficFunds {
                stake: wire::wxhopr(allocations.exit_traffic.capacity.stake),
                byte_capacity: allocations.exit_traffic.capacity.byte_capacity,
                bytes_from_exits: allocations.exit_traffic.bytes_from_exits,
                total_bytes_at_pix_price: allocations.total_bytes_at_pix_price(),
                price_per_byte: wire::wxhopr(allocations.exit_traffic.price_per_byte),
            },
        }
    }
}

impl From<&BalanceRecommendation> for gnosis_vpn_ipc::BalanceRecommendation {
    fn from(rec: &BalanceRecommendation) -> Self {
        Self {
            wxhopr: wire::wxhopr(rec.wxhopr),
            xdai: wire::xdai(rec.xdai),
            channel_stakes: wire::wxhopr(rec.channel_stakes),
            pix_pool: wire::wxhopr(rec.pix_pool),
            fee_to_start: wire::wxhopr(rec.fee_to_start),
            txs_to_start: rec.txs_to_start,
            xdai_fee_per_tx: wire::xdai(rec.xdai_fee_per_tx),
            topup_headroom: wire::wxhopr(rec.topup_headroom),
            pix_top_up: wire::wxhopr(rec.pix_top_up),
            pix_min_available: wire::wxhopr(rec.pix_min_available),
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
            pix_pool: Balance::<WxHOPR>::zero(),
            fee_to_start: Balance::<WxHOPR>::zero(),
            txs_to_start: 0,
            xdai_fee_per_tx: Balance::<XDai>::from(xdai),
            topup_headroom: Balance::<WxHOPR>::zero(),
            pix_top_up: Balance::<WxHOPR>::zero(),
            pix_min_available: Balance::<WxHOPR>::zero(),
            // One wei, so any more funds than two wei are Good.
            channel_max_balance: Balance::<WxHOPR>::from(1u64),
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
            closing: HashMap::new(),
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
            price_per_byte: Balance::zero(),
        }
    }

    #[test]
    fn traffic_empty_when_no_capacity_anywhere() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.traffic, FundingLevel::Empty);
    }

    /// Stands in for one wxHOPR, small enough that the stakes below fit a u64.
    const W: u64 = 1_000_000_000;

    /// A recommendation that knows the PIX refill and the largest channel, in `W`.
    fn ideal_sizes(pix: u64, channel: u64) -> BalanceRecommendation {
        BalanceRecommendation {
            pix_top_up: Balance::<WxHOPR>::from(pix * W),
            channel_max_balance: Balance::<WxHOPR>::from(channel * W),
            ..ideal(0, 0)
        }
    }

    /// Safe and PIX pool funds in `W`, at a PIX price in wei per byte.
    fn funded(safe: u64, pool: u64, price: u64) -> CapacityAllocations {
        CapacityAllocations {
            exit_traffic: ExitTrafficFunds {
                capacity: capacity(pool * W, 0),
                bytes_from_exits: 0,
                price_per_byte: Balance::from(price),
            },
            ..allocs(None, Capacity::default(), capacity(safe * W, 0))
        }
    }

    fn level_status(ideal: BalanceRecommendation, allocations: &CapacityAllocations) -> FundingStatus {
        to_funding_status(
            ideal,
            allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap()
    }

    #[test]
    fn the_pix_reserve_counts_against_the_refill_headroom() {
        let status = |reserved: u64| {
            to_funding_status(
                BalanceRecommendation {
                    topup_headroom: Balance::<WxHOPR>::from(80 * W),
                    ..ideal_sizes(35, 15)
                },
                &allocs(None, Capacity::default(), Capacity::default()),
                Balance::<XDai>::zero(),
                Balance::<WxHOPR>::from(100 * W),
                Balance::<WxHOPR>::from(reserved * W),
            )
            .unwrap()
        };
        assert_eq!(status(0).refill_shortfall, None);
        assert_eq!(status(35).refill_shortfall, Some(Balance::<WxHOPR>::from(15 * W)));
    }

    #[test]
    fn no_funding_status_without_the_channel_sizes() {
        let no_size = BalanceRecommendation {
            channel_max_balance: Balance::zero(),
            ..ideal_sizes(35, 22)
        };
        let status = to_funding_status(
            no_size,
            &funded(1_000, 0, 0),
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        );
        assert!(status.is_err());
    }

    #[test]
    fn empty_up_to_one_kept_channel_and_a_pix_refill() {
        let level = |safe| level_status(ideal_sizes(35, 22), &funded(safe, 0, 0)).traffic;
        assert_eq!(level(57), FundingLevel::Empty);
        assert_eq!(level(58), FundingLevel::Low);
    }

    #[test]
    fn low_up_to_twice_the_empty_funds() {
        let level = |safe| level_status(ideal_sizes(35, 22), &funded(safe, 0, 0)).traffic;
        assert_eq!(level(114), FundingLevel::Low);
        assert_eq!(level(115), FundingLevel::Good);
    }

    #[test]
    fn the_levels_follow_the_pix_refill_size() {
        let level = |pix| level_status(ideal_sizes(pix, 22), &funded(50, 0, 0)).traffic;
        assert_eq!(level(0), FundingLevel::Good);
        assert_eq!(level(10), FundingLevel::Low);
        assert_eq!(level(35), FundingLevel::Empty);
    }

    #[test]
    fn the_pix_pool_channels_and_unswept_node_funds_count_toward_the_level() {
        let allocations = CapacityAllocations {
            node: capacity(30 * W, 0),
            ..allocs(Some(capacity(30 * W, 0)), Capacity::default(), Capacity::default())
        };
        assert_eq!(
            level_status(ideal_sizes(35, 22), &allocations).traffic,
            FundingLevel::Low
        );
        let with_pool = CapacityAllocations {
            exit_traffic: valued_exit_funds(55 * W, 0),
            ..allocations
        };
        assert_eq!(
            level_status(ideal_sizes(35, 22), &with_pool).traffic,
            FundingLevel::Good
        );
    }

    #[test]
    fn wxhopr_deficit_tops_up_to_the_data_target_at_the_pix_price() {
        // 30 GiB at 10 wei per byte.
        let target = 10 * DATA_TARGET_BYTES;
        let status = level_status(ideal_sizes(35, 22), &funded(40, 10, 10));
        assert_eq!(status.traffic, FundingLevel::Empty);
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(target - 50 * W)));
    }

    #[test]
    fn wxhopr_deficit_is_just_past_low_without_a_pix_price() {
        let status = level_status(ideal_sizes(35, 22), &funded(50, 0, 0));
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(64 * W + 1)));
    }

    #[test]
    fn wxhopr_deficit_never_stops_short_of_good() {
        // 30 GiB at 1 wei per byte is about 32 W, under the 114 W that are still Low.
        let status = level_status(ideal_sizes(35, 22), &funded(50, 0, 1));
        assert_eq!(status.wxhopr_deficit, Some(Balance::<WxHOPR>::from(64 * W + 1)));
    }

    #[test]
    fn a_channel_holds_at_most_the_threshold_plus_one_top_up() {
        let funding = |initial: u64, threshold: u64, top_up: u64| hopr_strategy::channel_lifecycle::ResolvedFunding {
            initial_balance: Balance::from(initial),
            topup_balance: Balance::from(top_up),
            lower_balance_threshold: Balance::from(threshold),
            face_value: Balance::from(1u64),
        };
        assert_eq!(channel_max_balance(&funding(15, 7, 15)), Balance::<WxHOPR>::from(22u64));
        assert_eq!(channel_max_balance(&funding(40, 7, 15)), Balance::<WxHOPR>::from(40u64));
    }

    #[test]
    fn gas_empty_below_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(1_000_000_000_000_000_u64), // 0.001 xDAI < 0.0015 threshold
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.gas, FundingLevel::Empty);
    }

    #[test]
    fn gas_low_between_thresholds() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(2_000_000_000_000_000_u64), // 0.002 xDAI
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.gas, FundingLevel::Low);
    }

    #[test]
    fn gas_good_at_or_above_threshold() {
        let status = to_funding_status(
            ideal(0, 0),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI, at the boundary
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.gas, FundingLevel::Good);
    }

    #[test]
    fn balance_recommendation_from_edgli_totals_stakes_pool_and_fee() {
        let rec = edgli::strategy::BalanceRecommendation {
            channel_stakes: Balance::<WxHOPR>::from(800u64),
            pix_float: Balance::<WxHOPR>::from(200u64),
            fee_to_start: Balance::<WxHOPR>::from(10_000u64),
            txs_to_start: 3,
            xdai_fee_per_tx: Balance::<XDai>::from(100u64),
            xdai_fund_amount: Balance::<XDai>::from(5_000u64),
            topup_headroom: Balance::<WxHOPR>::from(42u64),
            channel_top_up: Balance::<WxHOPR>::from(15u64),
        };
        let mirrored: BalanceRecommendation = rec.into();
        assert_eq!(mirrored.wxhopr, Balance::<WxHOPR>::from(11_000u64));
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
        assert_eq!(mirrored.pix_pool, Balance::<WxHOPR>::from(200u64));
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
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.wxhopr_deficit, None);
    }

    #[test]
    fn xdai_deficit_none_when_gas_good() {
        let status = to_funding_status(
            ideal(0, 100),
            &CapacityAllocations::default(),
            Balance::<XDai>::from(3_500_000_000_000_000_u64),
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
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
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
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
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.gas, FundingLevel::Low);
        assert_eq!(
            status.xdai_deficit,
            Some(Balance::<XDai>::from(3_500_000_000_000_000_u64) - node_xdai),
            "deficit must be floored at the Low threshold, not the (lower) ideal recommendation"
        );
    }

    #[test]
    fn good_traffic_and_gas_when_well_funded() {
        let allocations = allocs(Some(capacity(100, 2 * GB)), capacity(0, 2 * GB), capacity(100, 2 * GB));
        let status = to_funding_status(
            ideal(100, 100),
            &allocations,
            Balance::<XDai>::from(3_500_000_000_000_000_u64), // 0.0035 xDAI — at the Good threshold
            Balance::<WxHOPR>::from(100u64),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
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
    fn total_at_pix_price_counts_every_balance() {
        let caps = CapacityAllocations {
            exit_traffic: ExitTrafficFunds::new(Balance::from(1000u64), Balance::from(10u64)),
            ..allocs(Some(capacity(5000, 0)), capacity(200, 0), capacity(800, 0))
        };
        assert_eq!(caps.total_bytes_at_pix_price(), 700);
        let wire = gnosis_vpn_ipc::CapacityAllocations::from(&caps);
        assert_eq!(wire.exit_traffic.bytes_from_exits, 100);
        assert_eq!(wire.exit_traffic.total_bytes_at_pix_price, 700);
        assert_eq!(wire.exit_traffic.price_per_byte, gnosis_vpn_ipc::Balance::from_wei(10));
    }

    #[test]
    fn total_at_pix_price_is_zero_without_a_pix_pool() {
        let caps = allocs(None, Capacity::default(), capacity(800, 0));
        assert_eq!(caps.total_bytes_at_pix_price(), 0);
    }

    #[test]
    fn total_at_pix_price_counts_in_flight_stake_folded_into_the_safe() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_exit(50, 100));
        let out = r.reconcile(with_exit(50, 60));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(100u64));
        assert_eq!(out.total_bytes_at_pix_price(), 150);
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

    #[test]
    fn fresher_pix_pool_drop_is_booked_as_paid_by_the_next_snapshot() {
        let mut r = CapacityReconciler::default();
        let published = r.reconcile(with_exit(100, 100));
        let fresher = r.with_pix_pool(&published, Balance::from(90u64));
        assert_eq!(total_stake(&fresher), Balance::<WxHOPR>::from(190u64));
        assert_eq!(fresher.exit_traffic.capacity.byte_capacity, 900);
        assert_eq!(fresher.exit_traffic.bytes_from_exits, 90);
        let out = r.reconcile(with_exit(90, 100));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(190u64));
        assert_eq!(out.safe.stake, Balance::<WxHOPR>::from(100u64));
    }

    #[test]
    fn fresher_pix_pool_rise_waits_for_the_next_snapshot() {
        let mut r = CapacityReconciler::default();
        let published = r.reconcile(with_exit(100, 100));
        let fresher = r.with_pix_pool(&published, Balance::from(150u64));
        assert_eq!(total_stake(&fresher), Balance::<WxHOPR>::from(200u64));
    }

    /// One channel to the fixture peer, open or closing, beside the Safe.
    fn with_channel(open: Option<u64>, closing: Option<u64>, safe: u64) -> CapacityAllocations {
        CapacityAllocations {
            closing: closing
                .map(|stake| (Address::from([1u8; 20]), capacity(stake, 0)))
                .into_iter()
                .collect(),
            ..allocs(open.map(cap10), Capacity::default(), cap10(safe))
        }
    }

    #[test]
    fn reconcile_keeps_the_total_through_a_channel_closure() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_channel(Some(100), None, 100));
        let closing = r.reconcile(with_channel(None, Some(100), 100));
        assert_eq!(total_stake(&closing), Balance::<WxHOPR>::from(200u64));
        assert_eq!(total_bytes(&closing), 2000);
        assert_eq!(closing.closing.values().next().unwrap().byte_capacity, 1000);
        // closed, but the stake isn't back in the Safe yet
        let between = r.reconcile(with_channel(None, None, 100));
        assert_eq!(total_stake(&between), Balance::<WxHOPR>::from(200u64));
        let back = r.reconcile(with_channel(None, None, 200));
        assert_eq!(total_stake(&back), Balance::<WxHOPR>::from(200u64));
        assert_eq!(back.safe.stake, Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_holds_a_closed_channel_while_another_starts_closing() {
        let mut r = CapacityReconciler::default();
        let mut before = with_channel(Some(100), None, 100);
        before.closing.insert(Address::from([2u8; 20]), capacity(100, 0));
        r.reconcile(before);
        // the closing channel is done but its stake isn't in the Safe yet; the open one starts closing
        let swapped = r.reconcile(with_channel(None, Some(100), 100));
        assert_eq!(total_stake(&swapped), Balance::<WxHOPR>::from(300u64));
        let back = r.reconcile(with_channel(None, Some(100), 200));
        assert_eq!(total_stake(&back), Balance::<WxHOPR>::from(300u64));
        assert_eq!(back.safe.stake, Balance::<WxHOPR>::from(200u64));
    }

    #[test]
    fn reconcile_still_passes_drainage_through_while_another_channel_starts_closing() {
        let mut r = CapacityReconciler::default();
        let mut both = with_channel(Some(100), None, 0);
        both.peer_allocations.insert(Address::from([2u8; 20]), cap10(50));
        r.reconcile(both);
        let mut out = with_channel(Some(80), None, 0);
        out.closing.insert(Address::from([2u8; 20]), capacity(50, 0));
        let out = r.reconcile(out);
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(130u64));
    }

    #[test]
    fn reconcile_passes_drainage_of_a_closing_channel_through() {
        let mut r = CapacityReconciler::default();
        r.reconcile(with_channel(None, Some(100), 100));
        let out = r.reconcile(with_channel(None, Some(70), 100));
        assert_eq!(total_stake(&out), Balance::<WxHOPR>::from(170u64));
    }

    #[test]
    fn closing_channels_count_toward_the_total_and_the_level() {
        let sized = BalanceRecommendation {
            channel_max_balance: Balance::<WxHOPR>::from(100u64),
            ..ideal(0, 0)
        };
        let open = with_channel(Some(30), None, 0);
        assert_eq!(level_status(sized, &open).traffic, FundingLevel::Empty);
        let mut closing = open;
        closing.closing.insert(Address::from([2u8; 20]), capacity(500, 5 * GB));
        assert_eq!(level_status(sized, &closing).traffic, FundingLevel::Good);
        assert_eq!(total_stake(&closing), Balance::<WxHOPR>::from(530u64));
    }

    #[test]
    fn closing_channels_go_out_with_the_open_ones() {
        let wire = gnosis_vpn_ipc::CapacityAllocations::from(&with_channel(None, Some(100), 0));
        assert_eq!(wire.peer_allocations.len(), 1);
    }

    fn peers(ids: &[u8]) -> HashSet<Address> {
        ids.iter().map(|id| Address::from([*id; 20])).collect()
    }

    #[test]
    fn channel_list_settles_after_three_equal_reads() {
        let start = Instant::now();
        let mut settle = ChannelSettle::new(start);
        for read in [
            peers(&[1, 2, 3]),
            peers(&[1, 2, 3, 4, 5]),
            peers(&[1, 2, 3, 4, 5, 6, 7, 8]),
        ] {
            settle.observe(read);
            assert!(!settle.is_settled(start));
        }
        let full = peers(&[1, 2, 3, 4, 5, 6, 7, 8]);
        settle.observe(full.clone());
        assert!(!settle.is_settled(start));
        settle.observe(full);
        assert!(settle.is_settled(start));
        settle.observe(peers(&[1]));
        assert!(settle.is_settled(start), "stays settled");
    }

    #[test]
    fn channel_list_settles_a_minute_after_the_start_even_if_it_keeps_changing() {
        let start = Instant::now();
        let mut settle = ChannelSettle::new(start);
        settle.observe(peers(&[1]));
        settle.observe(peers(&[1, 2]));
        assert!(!settle.is_settled(start + Duration::from_secs(59)));
        assert!(settle.is_settled(start + Duration::from_secs(60)));
    }

    #[test]
    fn channel_list_settles_a_minute_after_the_start_without_any_read() {
        let start = Instant::now();
        let settle = ChannelSettle::new(start);
        assert!(!settle.is_settled(start));
        assert!(settle.is_settled(start + Duration::from_secs(60)));
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
        let allocations = allocs(None, capacity(10, 2 * GB), Capacity::default());
        let status = to_funding_status(
            ideal_with_headroom(7),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(7 * WXHOPR),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.traffic, FundingLevel::Good);
        assert_eq!(status.refill_shortfall, None);
    }

    #[test]
    fn refill_shortfall_reports_the_gap_without_lowering_traffic() {
        // Traffic stays Good even though the Safe holds 2 of the 7 wxHOPR the next fund pass wants.
        let allocations = allocs(None, capacity(10, 2 * GB), Capacity::default());
        let status = to_funding_status(
            ideal_with_headroom(7),
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(2 * WXHOPR),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
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
        let allocations = allocs(None, capacity(10, 2 * GB), Capacity::default());
        let status = to_funding_status(
            both,
            &allocations,
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::from(7 * WXHOPR),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.refill_shortfall, Some(Balance::<WxHOPR>::from(4 * WXHOPR)));
    }

    #[test]
    fn refill_shortfall_is_reported_alongside_an_empty_pool() {
        let status = to_funding_status(
            ideal_with_headroom(1),
            &CapacityAllocations::default(),
            Balance::<XDai>::zero(),
            Balance::<WxHOPR>::zero(),
            Balance::<WxHOPR>::zero(),
        )
        .unwrap();
        assert_eq!(status.traffic, FundingLevel::Empty);
        assert_eq!(status.refill_shortfall, Some(Balance::<WxHOPR>::from(WXHOPR)));
    }
}
