//! The four delivery guarantees, over one unreliable datagram layer.
//!
//! Different game traffic wants opposite guarantees, and forcing one on all of it is the classic
//! mistake that makes a game feel bad under packet loss
//! ([ADR-0010](../../../docs/adr/0010-reliability-channels.md)):
//!
//! - A **position snapshot** must never wait. A lost one is worthless by the time it could be
//!   retransmitted, because a newer one has already arrived — and retransmitting it actively harms
//!   the game by delaying fresher data behind it.
//! - **"Player fired a rocket"** must arrive, and must arrive after "player picked up launcher".
//! - **Chat** must arrive, but blocking on a lost message helps nobody.
//! - **Voice** wants the newest only; an older packet arriving late should be dropped.
//!
//! The channel names are chosen so the right one is the obvious one. Putting position data on
//! `ReliableOrdered` is possible and is the single worst thing you can do to a game's feel.

use std::collections::{BTreeMap, HashSet};

use tempo_transport::Timestamp;

/// A delivery guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChannelKind {
    /// Best effort. May be lost, reordered or duplicated. The escape hatch.
    Unreliable = 0,
    /// Best effort, but stale messages are dropped on arrival. For snapshots, inputs and voice.
    UnreliableSequenced = 1,
    /// Delivered exactly once, in order, blocking on gaps. For spawns, despawns and RPCs.
    ReliableOrdered = 2,
    /// Delivered exactly once, in any order. For chat and independent events.
    ReliableUnordered = 3,
}

impl ChannelKind {
    /// Every channel, in wire order.
    pub const ALL: [ChannelKind; 4] = [
        ChannelKind::Unreliable,
        ChannelKind::UnreliableSequenced,
        ChannelKind::ReliableOrdered,
        ChannelKind::ReliableUnordered,
    ];

    /// Decodes from its wire value.
    pub const fn from_u8(v: u8) -> Option<ChannelKind> {
        match v {
            0 => Some(ChannelKind::Unreliable),
            1 => Some(ChannelKind::UnreliableSequenced),
            2 => Some(ChannelKind::ReliableOrdered),
            3 => Some(ChannelKind::ReliableUnordered),
            _ => None,
        }
    }

    /// True if this channel retransmits until acknowledged.
    pub const fn is_reliable(self) -> bool {
        matches!(
            self,
            ChannelKind::ReliableOrdered | ChannelKind::ReliableUnordered
        )
    }
}

/// A message ready to be written into a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// Which channel it belongs to.
    pub channel: ChannelKind,
    /// Per-channel message identifier.
    pub id: u32,
    /// The payload.
    pub payload: Vec<u8>,
}

/// A message that has been received and is ready for the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    /// Which channel it arrived on.
    pub channel: ChannelKind,
    /// Per-channel message identifier.
    pub id: u32,
    /// The payload.
    pub payload: Vec<u8>,
}

/// A reliable message awaiting acknowledgement.
#[derive(Debug, Clone)]
struct Pending {
    payload: Vec<u8>,
    last_sent: Option<Timestamp>,
    attempts: u32,
}

/// Retransmissions attempted at the eager interval before backoff begins.
///
/// Eight retries at roughly half the round trip covers several seconds of a badly lossy link. A
/// peer still unreachable after that is not congested, it is gone, and the connection layer should
/// time it out rather than the channel retrying harder.
const ATTEMPTS_BEFORE_BACKOFF: u32 = 8;

/// Largest power of two applied once backoff does begin.
const MAX_BACKOFF_SHIFT: u32 = 5;

/// Floor on the retry interval, in microseconds.
///
/// On a LAN the round trip is a fraction of a millisecond, and retrying at half of that would
/// resend a message many times per packet for no benefit.
const MIN_RETRY_US: u64 = 10_000;

/// The delay before a reliable message is retried.
///
/// # Why this is not a TCP-style retransmission timeout
///
/// The first implementation used the round-trip time as the retry interval and doubled it on every
/// failure, which is correct for bulk transfer and wrong here. Games are the opposite workload:
/// reliable messages are small and rare, packet rates are high, and a lost spawn or RPC must arrive
/// *soon* because gameplay is blocked on it. Under 33% loss that design took **three seconds** to
/// deliver twenty small messages, because the unluckiest message had backed off to hundreds of
/// milliseconds and an ordered channel makes everyone wait for it.
///
/// So the interval is roughly **half the round trip**, trading a little bandwidth for latency —
/// which is the right trade when the payload is a few bytes and the alternative is a visible stall.
/// Backoff still exists, but only after enough attempts to distinguish a lossy peer from a departed
/// one.
///
/// # Why the jitter is wide
///
/// A fixed interval resonates catastrophically with periodic loss. At a 50 ms interval with packets
/// every 20 ms, retries land every 60 ms — and a network dropping every third packet drops one every
/// 60 ms too, so every retransmission lands on a dropped packet and the channel starves forever.
/// That is not hypothetical; it is what the loss test found, and narrow jitter was not enough to fix
/// it — one message still hit packets 3, 9 and 21, all dropped. The spread is now half the interval.
///
/// Jitter is derived from the message id and attempt count rather than a generator, so timing stays
/// deterministic and replayable while being decorrelated from any periodic pattern in the network.
fn retry_delay_us(base_rto_us: u64, id: u32, attempts: u32) -> u64 {
    let shift = attempts
        .saturating_sub(ATTEMPTS_BEFORE_BACKOFF)
        .min(MAX_BACKOFF_SHIFT);
    let eager = (base_rto_us / 2).max(MIN_RETRY_US);
    let delay = eager.saturating_mul(1u64 << shift);
    let spread = delay / 2 + 1;
    let mixed = (id as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((attempts as u64).wrapping_mul(0x0000_0100_0000_01B3));
    delay + mixed % spread
}

/// Sending and receiving state for one channel.
#[derive(Debug, Clone)]
struct ChannelState {
    next_send_id: u32,

    /// Reliable messages not yet acknowledged, by id.
    unacked: BTreeMap<u32, Pending>,
    /// Unreliable messages queued for the next packet only.
    unreliable_queue: Vec<(u32, Vec<u8>)>,

    /// Receiver: next id to deliver on an ordered channel.
    next_expect_id: u32,
    /// Receiver: ordered messages held back waiting for a gap to fill.
    ordered_buffer: BTreeMap<u32, Vec<u8>>,
    /// Receiver: ids seen above the contiguous run, for unordered de-duplication.
    seen_above: HashSet<u32>,
    /// Receiver: highest id delivered on a sequenced channel.
    highest_seen: Option<u32>,
}

impl ChannelState {
    fn new() -> ChannelState {
        ChannelState {
            next_send_id: 0,
            unacked: BTreeMap::new(),
            unreliable_queue: Vec::new(),
            next_expect_id: 0,
            ordered_buffer: BTreeMap::new(),
            seen_above: HashSet::new(),
            highest_seen: None,
        }
    }
}

/// Manages all four channels for one connection.
#[derive(Debug, Clone)]
pub struct ChannelSet {
    channels: Vec<ChannelState>,
    /// Which messages went out in each packet, so an ack can retire them.
    in_packet: BTreeMap<u16, Vec<(ChannelKind, u32)>>,
    /// Cap on buffered out-of-order messages per channel, to bound memory.
    max_buffered: usize,
}

impl Default for ChannelSet {
    fn default() -> ChannelSet {
        ChannelSet::new()
    }
}

impl ChannelSet {
    /// Creates a set with default limits.
    pub fn new() -> ChannelSet {
        ChannelSet::with_max_buffered(1024)
    }

    /// Creates a set holding at most `max_buffered` out-of-order messages per channel.
    ///
    /// The cap is a denial-of-service bound, not a tuning knob: a peer that sends message 1,000,000
    /// and never sends the intervening ones would otherwise make the receiver buffer forever.
    pub fn with_max_buffered(max_buffered: usize) -> ChannelSet {
        ChannelSet {
            channels: ChannelKind::ALL
                .iter()
                .map(|_| ChannelState::new())
                .collect(),
            in_packet: BTreeMap::new(),
            max_buffered,
        }
    }

    fn state(&mut self, kind: ChannelKind) -> &mut ChannelState {
        &mut self.channels[kind as usize]
    }

    /// Queues a message for delivery, returning its per-channel id.
    pub fn send(&mut self, kind: ChannelKind, payload: Vec<u8>) -> u32 {
        let s = self.state(kind);
        let id = s.next_send_id;
        s.next_send_id = s.next_send_id.wrapping_add(1);
        if kind.is_reliable() {
            s.unacked.insert(
                id,
                Pending {
                    payload,
                    last_sent: None,
                    attempts: 0,
                },
            );
        } else {
            s.unreliable_queue.push((id, payload));
        }
        id
    }

    /// Collects messages to write into the packet numbered `packet_seq`.
    ///
    /// Reliable messages are included if never sent, or if their retransmission timeout has
    /// elapsed. Unreliable messages are included once and then dropped — retransmitting them is
    /// precisely what their channel exists to avoid.
    pub fn packetize(
        &mut self,
        packet_seq: u16,
        now: Timestamp,
        rto_us: u64,
        mut budget_bytes: usize,
    ) -> Vec<OutgoingMessage> {
        let mut out = Vec::new();
        let mut recorded: Vec<(ChannelKind, u32)> = Vec::new();

        // Reliable channels first: their content is the traffic that must not be starved by a
        // burst of snapshots.
        for kind in [
            ChannelKind::ReliableOrdered,
            ChannelKind::ReliableUnordered,
            ChannelKind::UnreliableSequenced,
            ChannelKind::Unreliable,
        ] {
            let max_buffered = self.max_buffered;
            let s = &mut self.channels[kind as usize];
            let _ = max_buffered;

            if kind.is_reliable() {
                for (&id, pending) in s.unacked.iter_mut() {
                    let due = match pending.last_sent {
                        None => true,
                        Some(t) => now.since(t) >= retry_delay_us(rto_us, id, pending.attempts),
                    };
                    if !due {
                        continue;
                    }
                    if pending.payload.len() > budget_bytes {
                        break;
                    }
                    budget_bytes -= pending.payload.len();
                    // `attempts` counts *retransmissions*, so the initial send leaves it at zero
                    // and the first retry waits one RTO rather than two.
                    if pending.last_sent.is_some() {
                        pending.attempts = pending.attempts.saturating_add(1);
                    }
                    pending.last_sent = Some(now);
                    out.push(OutgoingMessage {
                        channel: kind,
                        id,
                        payload: pending.payload.clone(),
                    });
                    recorded.push((kind, id));
                }
            } else {
                let queued = core::mem::take(&mut s.unreliable_queue);
                for (id, payload) in queued {
                    if payload.len() > budget_bytes {
                        // Dropped rather than deferred. An unreliable message that missed its
                        // packet is already stale.
                        continue;
                    }
                    budget_bytes -= payload.len();
                    out.push(OutgoingMessage {
                        channel: kind,
                        id,
                        payload,
                    });
                }
            }
        }

        if !recorded.is_empty() {
            self.in_packet.insert(packet_seq, recorded);
        }
        out
    }

    /// Retires the reliable messages carried by an acknowledged packet.
    pub fn on_packet_acked(&mut self, packet_seq: u16) {
        let Some(entries) = self.in_packet.remove(&packet_seq) else {
            return;
        };
        for (kind, id) in entries {
            self.channels[kind as usize].unacked.remove(&id);
        }
    }

    /// Forgets bookkeeping for a packet known to be lost.
    ///
    /// Its messages stay unacknowledged and will be retransmitted when their timeout elapses.
    pub fn on_packet_lost(&mut self, packet_seq: u16) {
        self.in_packet.remove(&packet_seq);
    }

    /// Accepts a received message and returns whatever is now deliverable.
    ///
    /// Returns a vector because filling a gap on an ordered channel can release a run of buffered
    /// messages at once.
    pub fn on_receive(
        &mut self,
        kind: ChannelKind,
        id: u32,
        payload: Vec<u8>,
    ) -> Vec<IncomingMessage> {
        let max_buffered = self.max_buffered;
        let s = &mut self.channels[kind as usize];
        let mut out = Vec::new();

        match kind {
            ChannelKind::Unreliable => {
                out.push(IncomingMessage {
                    channel: kind,
                    id,
                    payload,
                });
            }

            ChannelKind::UnreliableSequenced => {
                // Newest only. A late arrival is stale by definition and is discarded, which is
                // exactly the behaviour that keeps a lossy link feeling responsive.
                let newer = match s.highest_seen {
                    None => true,
                    Some(h) => id.wrapping_sub(h) != 0 && id.wrapping_sub(h) < u32::MAX / 2,
                };
                if newer {
                    s.highest_seen = Some(id);
                    out.push(IncomingMessage {
                        channel: kind,
                        id,
                        payload,
                    });
                }
            }

            ChannelKind::ReliableUnordered => {
                if id < s.next_expect_id || s.seen_above.contains(&id) {
                    return out; // duplicate
                }
                out.push(IncomingMessage {
                    channel: kind,
                    id,
                    payload,
                });
                if id == s.next_expect_id {
                    s.next_expect_id += 1;
                    while s.seen_above.remove(&s.next_expect_id) {
                        s.next_expect_id += 1;
                    }
                } else if s.seen_above.len() < max_buffered {
                    s.seen_above.insert(id);
                }
            }

            ChannelKind::ReliableOrdered => {
                if id < s.next_expect_id || s.ordered_buffer.contains_key(&id) {
                    return out; // duplicate
                }
                if id == s.next_expect_id {
                    out.push(IncomingMessage {
                        channel: kind,
                        id,
                        payload,
                    });
                    s.next_expect_id += 1;
                    // Filling a gap can release a whole run at once.
                    while let Some(next) = s.ordered_buffer.remove(&s.next_expect_id) {
                        out.push(IncomingMessage {
                            channel: kind,
                            id: s.next_expect_id,
                            payload: next,
                        });
                        s.next_expect_id += 1;
                    }
                } else if s.ordered_buffer.len() < max_buffered {
                    s.ordered_buffer.insert(id, payload);
                }
                // Beyond the cap the message is dropped. It will be retransmitted, and refusing to
                // buffer unboundedly is what stops a peer from exhausting memory with a gap.
            }
        }
        out
    }

    /// Number of reliable messages still awaiting acknowledgement on a channel.
    pub fn unacked_count(&self, kind: ChannelKind) -> usize {
        self.channels[kind as usize].unacked.len()
    }

    /// Number of messages buffered waiting for a gap to fill.
    pub fn buffered_count(&self, kind: ChannelKind) -> usize {
        let s = &self.channels[kind as usize];
        s.ordered_buffer.len() + s.seen_above.len()
    }

    /// True if every reliable message has been acknowledged.
    pub fn is_fully_acked(&self) -> bool {
        self.channels.iter().all(|c| c.unacked.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    const BIG: usize = 100_000;

    #[test]
    fn reliable_messages_retransmit_until_acknowledged() {
        let mut s = ChannelSet::new();
        s.send(ChannelKind::ReliableOrdered, b"important".to_vec());

        let first = s.packetize(0, at(0), 100_000, BIG);
        assert_eq!(first.len(), 1);

        // Not yet due: the eager interval is half the round trip, so 50 ms of a 100 ms RTO.
        assert!(s.packetize(1, at(20), 100_000, BIG).is_empty());

        // Past the interval plus its jitter, which is bounded at half again.
        let retry = s.packetize(2, at(200), 100_000, BIG);
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].payload, b"important");

        s.on_packet_acked(2);
        assert!(s.packetize(3, at(500), 100_000, BIG).is_empty());
        assert!(s.is_fully_acked());
    }

    #[test]
    fn unreliable_messages_are_sent_once_and_never_retransmitted() {
        // Retransmitting a stale snapshot actively harms the game by delaying fresher data.
        let mut s = ChannelSet::new();
        s.send(ChannelKind::UnreliableSequenced, b"snapshot".to_vec());
        assert_eq!(s.packetize(0, at(0), 100_000, BIG).len(), 1);
        assert!(s.packetize(1, at(1000), 100_000, BIG).is_empty());
        assert!(s.is_fully_acked(), "unreliable messages are never tracked");
    }

    #[test]
    fn ordered_delivery_blocks_on_a_gap_then_releases_the_run() {
        let mut s = ChannelSet::new();
        let k = ChannelKind::ReliableOrdered;

        assert_eq!(s.on_receive(k, 0, b"a".to_vec()).len(), 1);
        // 1 is missing, so 2 and 3 are held.
        assert!(s.on_receive(k, 2, b"c".to_vec()).is_empty());
        assert!(s.on_receive(k, 3, b"d".to_vec()).is_empty());
        assert_eq!(s.buffered_count(k), 2);

        let released = s.on_receive(k, 1, b"b".to_vec());
        let payloads: Vec<&[u8]> = released.iter().map(|m| m.payload.as_slice()).collect();
        assert_eq!(payloads, vec![b"b".as_ref(), b"c".as_ref(), b"d".as_ref()]);
        assert_eq!(s.buffered_count(k), 0);
    }

    #[test]
    fn unordered_delivery_does_not_block() {
        let mut s = ChannelSet::new();
        let k = ChannelKind::ReliableUnordered;
        assert_eq!(
            s.on_receive(k, 5, b"e".to_vec()).len(),
            1,
            "delivered immediately"
        );
        assert_eq!(s.on_receive(k, 0, b"a".to_vec()).len(), 1);
        assert_eq!(s.on_receive(k, 2, b"c".to_vec()).len(), 1);
    }

    #[test]
    fn duplicates_are_suppressed_on_reliable_channels() {
        // Networks duplicate, and reliable channels also retransmit, so this is routine.
        for k in [ChannelKind::ReliableOrdered, ChannelKind::ReliableUnordered] {
            let mut s = ChannelSet::new();
            assert_eq!(s.on_receive(k, 0, b"x".to_vec()).len(), 1);
            assert!(
                s.on_receive(k, 0, b"x".to_vec()).is_empty(),
                "{k:?} redelivered a duplicate"
            );
            assert_eq!(s.on_receive(k, 1, b"y".to_vec()).len(), 1);
            assert!(s.on_receive(k, 1, b"y".to_vec()).is_empty());
        }
    }

    #[test]
    fn sequenced_delivery_drops_stale_arrivals() {
        let mut s = ChannelSet::new();
        let k = ChannelKind::UnreliableSequenced;
        assert_eq!(s.on_receive(k, 10, b"new".to_vec()).len(), 1);
        assert!(
            s.on_receive(k, 9, b"old".to_vec()).is_empty(),
            "stale must be discarded"
        );
        assert!(s.on_receive(k, 10, b"same".to_vec()).is_empty());
        assert_eq!(s.on_receive(k, 11, b"newer".to_vec()).len(), 1);
    }

    #[test]
    fn plain_unreliable_delivers_everything_including_duplicates() {
        let mut s = ChannelSet::new();
        let k = ChannelKind::Unreliable;
        assert_eq!(s.on_receive(k, 0, b"x".to_vec()).len(), 1);
        assert_eq!(
            s.on_receive(k, 0, b"x".to_vec()).len(),
            1,
            "no de-duplication here"
        );
        assert_eq!(s.on_receive(k, 5, b"y".to_vec()).len(), 1);
    }

    #[test]
    fn a_lost_packet_leaves_its_messages_pending() {
        let mut s = ChannelSet::new();
        s.send(ChannelKind::ReliableOrdered, b"m".to_vec());
        assert_eq!(s.packetize(0, at(0), 100_000, BIG).len(), 1);

        s.on_packet_lost(0);
        assert_eq!(s.unacked_count(ChannelKind::ReliableOrdered), 1);
        assert_eq!(
            s.packetize(1, at(200), 100_000, BIG).len(),
            1,
            "retransmitted"
        );
    }

    #[test]
    fn acking_a_packet_retires_exactly_its_messages() {
        let mut s = ChannelSet::new();
        s.send(ChannelKind::ReliableOrdered, b"first".to_vec());
        assert_eq!(s.packetize(0, at(0), 100_000, BIG).len(), 1);
        s.send(ChannelKind::ReliableOrdered, b"second".to_vec());
        assert_eq!(s.packetize(1, at(10), 100_000, BIG).len(), 1);

        s.on_packet_acked(0);
        assert_eq!(
            s.unacked_count(ChannelKind::ReliableOrdered),
            1,
            "only the second remains"
        );
        s.on_packet_acked(1);
        assert!(s.is_fully_acked());
    }

    #[test]
    fn the_budget_is_respected_and_reliable_traffic_goes_first() {
        let mut s = ChannelSet::new();
        s.send(ChannelKind::ReliableOrdered, vec![0u8; 40]);
        s.send(ChannelKind::UnreliableSequenced, vec![0u8; 40]);

        let out = s.packetize(0, at(0), 100_000, 50);
        assert_eq!(out.len(), 1, "only one fits");
        assert_eq!(
            out[0].channel,
            ChannelKind::ReliableOrdered,
            "reliable traffic must not be starved by a burst of snapshots"
        );
    }

    #[test]
    fn out_of_order_buffering_is_bounded() {
        // A peer that sends message 1,000,000 and withholds the rest must not make the receiver
        // buffer without limit.
        let mut s = ChannelSet::with_max_buffered(8);
        let k = ChannelKind::ReliableOrdered;
        for id in 1..100u32 {
            s.on_receive(k, id, vec![0u8; 16]);
        }
        assert!(s.buffered_count(k) <= 8, "buffered {}", s.buffered_count(k));
    }

    #[test]
    fn channel_kinds_round_trip_through_their_wire_value() {
        for k in ChannelKind::ALL {
            assert_eq!(ChannelKind::from_u8(k as u8), Some(k));
        }
        assert_eq!(ChannelKind::from_u8(4), None);
        assert!(ChannelKind::ReliableOrdered.is_reliable());
        assert!(!ChannelKind::UnreliableSequenced.is_reliable());
    }

    #[test]
    fn retransmission_does_not_resonate_with_periodic_loss() {
        // The bug the loss test found: with a fixed retry interval, retries land on a periodic
        // multiple that can coincide exactly with a periodic drop pattern, starving the channel
        // forever. Backoff plus jitter must spread retries across different packet indices.
        let mut s = ChannelSet::new();
        s.send(ChannelKind::ReliableOrdered, b"m".to_vec());

        let mut sent_on: Vec<u16> = Vec::new();
        for packet in 0..80u16 {
            let now = at(packet as u64 * 20);
            if !s.packetize(packet, now, 50_000, BIG).is_empty() {
                sent_on.push(packet);
            }
        }
        assert!(
            sent_on.len() >= 4,
            "expected several retries, got {sent_on:?}"
        );
        let residues: std::collections::HashSet<u16> = sent_on.iter().map(|p| p % 3).collect();
        assert!(
            residues.len() > 1,
            "every retry landed on the same phase ({sent_on:?}); a periodic drop would starve it"
        );
    }

    #[test]
    fn a_full_exchange_delivers_everything_reliable_under_loss() {
        // The integration property: with every third packet dropped, reliable channels still
        // deliver every message exactly once and in order.
        let mut sender = ChannelSet::new();
        let mut receiver = ChannelSet::new();
        let k = ChannelKind::ReliableOrdered;
        for i in 0..20u32 {
            sender.send(k, i.to_le_bytes().to_vec());
        }

        let mut delivered: Vec<u32> = Vec::new();
        for packet in 0..60u16 {
            let now = at(packet as u64 * 20);
            let msgs = sender.packetize(packet, now, 50_000, BIG);
            if packet % 3 == 0 {
                sender.on_packet_lost(packet);
                continue;
            }
            for m in msgs {
                for got in receiver.on_receive(m.channel, m.id, m.payload) {
                    delivered.push(u32::from_le_bytes(got.payload[..4].try_into().unwrap()));
                }
            }
            sender.on_packet_acked(packet);
        }

        assert_eq!(
            delivered,
            (0..20).collect::<Vec<u32>>(),
            "exactly once, in order"
        );
        assert!(sender.is_fully_acked());
    }
}
