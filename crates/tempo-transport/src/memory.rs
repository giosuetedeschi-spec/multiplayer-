//! In-memory transport, with optional simulated link conditions.
//!
//! With [`LinkConditions::PERFECT`] this is a loopback: datagrams arrive instantly, in order, and
//! never drop. That serves tests, single-player, and local co-op with no code change at the layers
//! above.
//!
//! With anything else it is a **simulated link**, and that is the point. Prediction, reconciliation
//! and rollback only reveal their bugs under latency and loss, which are precisely the conditions
//! that are painful to reproduce against a real network. Here they are a struct and a seed, so a
//! failure at 200 ms RTT with 10% loss is reproducible on every machine and in CI.
//!
//! Nothing here reads a clock. Delivery times are computed from the `now` the caller passes, so a
//! test can step time in whatever increments it likes and get identical results every run.

use std::collections::BinaryHeap;
use std::sync::{Arc, Mutex};

use crate::rng::Rng;
use crate::{PeerId, Received, Timestamp, Transport, TransportError, MTU};

/// How a simulated link mistreats datagrams.
///
/// Probabilities are parts per million as integers, never floats — a float here would reintroduce
/// the platform variation the rest of the project exists to eliminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkConditions {
    /// One-way base latency, in microseconds. Round-trip time is roughly twice this.
    pub latency_us: u64,
    /// Maximum extra delay added per datagram, uniformly distributed over `0..=jitter_us`.
    pub jitter_us: u64,
    /// Probability a datagram is dropped, in parts per million.
    pub loss_ppm: u32,
    /// Probability a datagram is delivered twice, in parts per million.
    ///
    /// Duplicates are real on the internet and a classic source of bugs in code that assumes
    /// exactly-once arrival, so the simulator produces them rather than pretending they do not
    /// happen.
    pub duplicate_ppm: u32,
    /// Probability a datagram is held back far enough to arrive out of order.
    pub reorder_ppm: u32,
    /// Extra delay applied to a reordered datagram, in microseconds.
    pub reorder_extra_us: u64,
}

impl LinkConditions {
    /// No latency, no loss, no reordering. Instant in-order delivery.
    pub const PERFECT: LinkConditions = LinkConditions {
        latency_us: 0,
        jitter_us: 0,
        loss_ppm: 0,
        duplicate_ppm: 0,
        reorder_ppm: 0,
        reorder_extra_us: 0,
    };

    /// A local network: sub-millisecond, essentially lossless.
    pub const LAN: LinkConditions = LinkConditions {
        latency_us: 500,
        jitter_us: 200,
        loss_ppm: 100,
        duplicate_ppm: 0,
        reorder_ppm: 0,
        reorder_extra_us: 0,
    };

    /// A good broadband connection: about 40 ms round trip, minimal loss.
    pub const BROADBAND: LinkConditions = LinkConditions {
        latency_us: 20_000,
        jitter_us: 5_000,
        loss_ppm: 1_000,
        duplicate_ppm: 100,
        reorder_ppm: 1_000,
        reorder_extra_us: 15_000,
    };

    /// Congested Wi-Fi: about 120 ms round trip, 3% loss, visible jitter.
    pub const POOR_WIFI: LinkConditions = LinkConditions {
        latency_us: 60_000,
        jitter_us: 40_000,
        loss_ppm: 30_000,
        duplicate_ppm: 2_000,
        reorder_ppm: 20_000,
        reorder_extra_us: 60_000,
    };

    /// A bad mobile connection: about 300 ms round trip, 10% loss, heavy jitter.
    ///
    /// This is the profile worth running prediction and rollback against before shipping.
    pub const MOBILE: LinkConditions = LinkConditions {
        latency_us: 150_000,
        jitter_us: 80_000,
        loss_ppm: 100_000,
        duplicate_ppm: 5_000,
        reorder_ppm: 50_000,
        reorder_extra_us: 120_000,
    };

    /// A symmetric link with the given round-trip time and loss, and no reordering.
    pub const fn rtt(rtt_ms: u64, loss_percent: u32) -> LinkConditions {
        LinkConditions {
            latency_us: rtt_ms * 500, // half the round trip, in microseconds
            jitter_us: 0,
            loss_ppm: loss_percent.saturating_mul(10_000),
            duplicate_ppm: 0,
            reorder_ppm: 0,
            reorder_extra_us: 0,
        }
    }

    /// True if this link delivers everything instantly and in order.
    pub const fn is_perfect(&self) -> bool {
        self.latency_us == 0
            && self.jitter_us == 0
            && self.loss_ppm == 0
            && self.duplicate_ppm == 0
            && self.reorder_ppm == 0
    }
}

impl Default for LinkConditions {
    fn default() -> LinkConditions {
        LinkConditions::PERFECT
    }
}

/// A datagram waiting to be delivered.
///
/// Ordered by delivery time, then by sequence so that datagrams scheduled for the same instant
/// arrive in send order. Without the sequence tiebreak, ordering would depend on heap internals and
/// runs would not be reproducible.
#[derive(Debug, PartialEq, Eq)]
struct InFlight {
    deliver_at: u64,
    seq: u64,
    from: PeerId,
    to: PeerId,
    data: Vec<u8>,
}

impl Ord for InFlight {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed: BinaryHeap is a max-heap and we want the earliest delivery first.
        other
            .deliver_at
            .cmp(&self.deliver_at)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for InFlight {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
struct NetworkInner {
    queue: BinaryHeap<InFlight>,
    conditions: LinkConditions,
    rng: Rng,
    next_seq: u64,
    sent: u64,
    dropped: u64,
    duplicated: u64,
}

/// A shared in-memory network that endpoints send through.
///
/// Cloning shares the same network, which is how two endpoints reach each other.
#[derive(Debug, Clone)]
pub struct MemoryNetwork {
    inner: Arc<Mutex<NetworkInner>>,
}

impl Default for MemoryNetwork {
    fn default() -> MemoryNetwork {
        MemoryNetwork::new()
    }
}

impl MemoryNetwork {
    /// A network with perfect conditions.
    pub fn new() -> MemoryNetwork {
        MemoryNetwork::with_conditions(LinkConditions::PERFECT, 0x5EED)
    }

    /// A network with the given conditions and PRNG seed.
    ///
    /// The seed fully determines which datagrams are dropped, duplicated and reordered, so a
    /// failing test reproduces exactly.
    pub fn with_conditions(conditions: LinkConditions, seed: u64) -> MemoryNetwork {
        MemoryNetwork {
            inner: Arc::new(Mutex::new(NetworkInner {
                queue: BinaryHeap::new(),
                conditions,
                rng: Rng::new(seed),
                next_seq: 0,
                sent: 0,
                dropped: 0,
                duplicated: 0,
            })),
        }
    }

    /// Creates an endpoint bound to `id`.
    pub fn endpoint(&self, id: PeerId) -> MemoryTransport {
        MemoryTransport {
            network: self.clone(),
            id,
        }
    }

    /// Replaces the link conditions. Datagrams already in flight keep their schedule.
    pub fn set_conditions(&self, conditions: LinkConditions) {
        self.lock().conditions = conditions;
    }

    /// The current link conditions.
    pub fn conditions(&self) -> LinkConditions {
        self.lock().conditions
    }

    /// Datagrams accepted, dropped, and duplicated so far.
    pub fn stats(&self) -> NetworkStats {
        let i = self.lock();
        NetworkStats {
            sent: i.sent,
            dropped: i.dropped,
            duplicated: i.duplicated,
        }
    }

    /// Number of datagrams still in flight.
    pub fn in_flight(&self) -> usize {
        self.lock().queue.len()
    }

    /// Discards everything in flight.
    pub fn clear(&self) {
        self.lock().queue.clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, NetworkInner> {
        // A poisoned lock means a test panicked mid-send. Recovering keeps the original panic as
        // the reported failure instead of burying it under a lock error.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn enqueue(&self, from: PeerId, to: PeerId, data: &[u8], now: Timestamp) {
        let mut i = self.lock();
        i.sent += 1;

        let c = i.conditions;
        if i.rng.chance(c.loss_ppm) {
            i.dropped += 1;
            return;
        }

        let duplicate = i.rng.chance(c.duplicate_ppm);
        let copies = if duplicate { 2 } else { 1 };
        if duplicate {
            i.duplicated += 1;
        }

        for _ in 0..copies {
            let mut delay = c.latency_us;
            if c.jitter_us > 0 {
                delay += i.rng.below(c.jitter_us + 1);
            }
            if i.rng.chance(c.reorder_ppm) {
                delay += c.reorder_extra_us;
            }
            let seq = i.next_seq;
            i.next_seq += 1;
            i.queue.push(InFlight {
                deliver_at: now.as_micros().saturating_add(delay),
                seq,
                from,
                to,
                data: data.to_vec(),
            });
        }
    }

    fn dequeue(&self, to: PeerId, now: Timestamp) -> Option<Received> {
        let mut i = self.lock();
        // The heap is ordered globally by delivery time, but a datagram at the front may be
        // addressed elsewhere. Pop deliverable entries, keep the ones for other peers aside, and
        // put them back — the queue is small enough that this is cheaper than a heap per peer.
        let mut parked: Vec<InFlight> = Vec::new();
        let found = loop {
            match i.queue.peek() {
                Some(f) if f.deliver_at <= now.as_micros() => {
                    let f = i.queue.pop().expect("peeked");
                    if f.to == to {
                        break Some(f);
                    }
                    parked.push(f);
                }
                _ => break None,
            }
        };
        for f in parked {
            i.queue.push(f);
        }
        found.map(|f| Received {
            from: f.from,
            data: f.data,
        })
    }
}

/// Counters describing what a [`MemoryNetwork`] has done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkStats {
    /// Datagrams accepted for delivery.
    pub sent: u64,
    /// Datagrams dropped by simulated loss.
    pub dropped: u64,
    /// Datagrams delivered twice by simulated duplication.
    pub duplicated: u64,
}

/// An endpoint on a [`MemoryNetwork`].
#[derive(Debug, Clone)]
pub struct MemoryTransport {
    network: MemoryNetwork,
    id: PeerId,
}

impl MemoryTransport {
    /// The network this endpoint belongs to.
    pub fn network(&self) -> &MemoryNetwork {
        &self.network
    }
}

impl Transport for MemoryTransport {
    fn send(&mut self, to: PeerId, data: &[u8], now: Timestamp) -> Result<(), TransportError> {
        if data.len() > MTU {
            return Err(TransportError::PayloadTooLarge {
                len: data.len(),
                mtu: MTU,
            });
        }
        self.network.enqueue(self.id, to, data, now);
        Ok(())
    }

    fn recv(&mut self, now: Timestamp) -> Option<Received> {
        self.network.dequeue(self.id, now)
    }

    fn local_peer(&self) -> PeerId {
        self.id
    }
}

/// Creates a connected pair of endpoints on a perfect link.
pub fn loopback_pair() -> (MemoryTransport, MemoryTransport) {
    let net = MemoryNetwork::new();
    (net.endpoint(PeerId(0)), net.endpoint(PeerId(1)))
}

/// Creates a connected pair of endpoints on a simulated link.
pub fn simulated_pair(conditions: LinkConditions, seed: u64) -> (MemoryTransport, MemoryTransport) {
    let net = MemoryNetwork::with_conditions(conditions, seed);
    (net.endpoint(PeerId(0)), net.endpoint(PeerId(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drain;

    const A: PeerId = PeerId(0);
    const B: PeerId = PeerId(1);

    #[test]
    fn a_perfect_link_delivers_immediately_and_in_order() {
        let (mut a, mut b) = loopback_pair();
        let t = Timestamp::ZERO;
        for i in 0..10u8 {
            a.send(B, &[i], t).unwrap();
        }
        let got: Vec<u8> = drain(&mut b, t).into_iter().map(|r| r.data[0]).collect();
        assert_eq!(got, (0..10).collect::<Vec<u8>>());
    }

    #[test]
    fn datagrams_only_reach_their_addressee() {
        let net = MemoryNetwork::new();
        let mut a = net.endpoint(A);
        let mut b = net.endpoint(B);
        let mut c = net.endpoint(PeerId(2));
        let t = Timestamp::ZERO;

        a.send(B, b"for-b", t).unwrap();
        a.send(PeerId(2), b"for-c", t).unwrap();

        assert_eq!(drain(&mut b, t).len(), 1);
        assert_eq!(drain(&mut c, t).len(), 1);
        assert!(drain(&mut a, t).is_empty());
    }

    #[test]
    fn latency_holds_datagrams_until_their_time() {
        let (mut a, mut b) = simulated_pair(LinkConditions::rtt(100, 0), 1);
        a.send(B, b"hello", Timestamp::ZERO).unwrap();

        assert!(
            b.recv(Timestamp::from_millis(49)).is_none(),
            "arrived early"
        );
        assert!(
            b.recv(Timestamp::from_millis(50)).is_some(),
            "one-way is half the RTT"
        );
    }

    #[test]
    fn the_same_seed_reproduces_the_same_run() {
        // The property that makes this crate useful: a failure under bad conditions must reproduce
        // exactly, on any machine, every time.
        let run = |seed: u64| -> Vec<(u64, Vec<u8>)> {
            let (mut a, mut b) = simulated_pair(LinkConditions::MOBILE, seed);
            let mut received = Vec::new();
            for tick in 0..200u64 {
                let now = Timestamp::from_millis(tick * 16);
                a.send(B, &tick.to_le_bytes(), now).unwrap();
                for r in drain(&mut b, now) {
                    received.push((tick, r.data));
                }
            }
            received
        };
        assert_eq!(run(12345), run(12345));
        assert_ne!(
            run(12345),
            run(999),
            "different seeds must explore different behaviour"
        );
    }

    #[test]
    fn loss_actually_drops_datagrams() {
        let (mut a, mut b) = simulated_pair(LinkConditions::rtt(0, 10), 42);
        let t = Timestamp::ZERO;
        for i in 0..1000u32 {
            a.send(B, &i.to_le_bytes(), t).unwrap();
        }
        let got = drain(&mut b, t).len();
        assert!(
            (850..=950).contains(&got),
            "expected roughly 900 of 1000, got {got}"
        );

        let stats = a.network().stats();
        assert_eq!(stats.sent, 1000);
        assert_eq!(stats.sent - stats.dropped, got as u64);
    }

    #[test]
    fn zero_loss_drops_nothing_at_all() {
        // Exactness matters: a link configured lossless must be lossless, not almost.
        let (mut a, mut b) = simulated_pair(LinkConditions::rtt(20, 0), 7);
        let t = Timestamp::ZERO;
        for i in 0..2000u32 {
            a.send(B, &i.to_le_bytes(), t).unwrap();
        }
        let got = drain(&mut b, Timestamp::from_millis(100)).len();
        assert_eq!(got, 2000);
        assert_eq!(a.network().stats().dropped, 0);
    }

    #[test]
    fn jitter_and_reordering_scramble_arrival_order() {
        let conditions = LinkConditions {
            latency_us: 20_000,
            jitter_us: 30_000,
            reorder_ppm: 200_000,
            reorder_extra_us: 50_000,
            ..LinkConditions::PERFECT
        };
        let (mut a, mut b) = simulated_pair(conditions, 99);
        let t = Timestamp::ZERO;
        for i in 0..100u8 {
            a.send(B, &[i], t).unwrap();
        }
        let got: Vec<u8> = drain(&mut b, Timestamp::from_millis(500))
            .into_iter()
            .map(|r| r.data[0])
            .collect();

        assert_eq!(
            got.len(),
            100,
            "nothing should be lost with zero loss configured"
        );
        let sorted: Vec<u8> = (0..100).collect();
        assert_ne!(got, sorted, "jitter and reordering must actually reorder");
        let mut resorted = got.clone();
        resorted.sort_unstable();
        assert_eq!(
            resorted, sorted,
            "reordering must not lose or invent datagrams"
        );
    }

    #[test]
    fn duplicates_are_delivered_twice() {
        // Real networks duplicate. Code that assumes exactly-once arrival should fail here rather
        // than in production.
        let conditions = LinkConditions {
            duplicate_ppm: 1_000_000,
            ..LinkConditions::PERFECT
        };
        let (mut a, mut b) = simulated_pair(conditions, 5);
        let t = Timestamp::ZERO;
        a.send(B, b"x", t).unwrap();
        assert_eq!(drain(&mut b, t).len(), 2);
        assert_eq!(a.network().stats().duplicated, 1);
    }

    #[test]
    fn arrival_is_ordered_by_delivery_time_not_send_time() {
        let net = MemoryNetwork::with_conditions(LinkConditions::PERFECT, 1);
        let mut a = net.endpoint(A);
        let mut b = net.endpoint(B);

        // Send "late" first under high latency, then "early" with none.
        net.set_conditions(LinkConditions::rtt(100, 0));
        a.send(B, b"late", Timestamp::ZERO).unwrap();
        net.set_conditions(LinkConditions::PERFECT);
        a.send(B, b"early", Timestamp::ZERO).unwrap();

        let got: Vec<Vec<u8>> = drain(&mut b, Timestamp::from_millis(100))
            .into_iter()
            .map(|r| r.data)
            .collect();
        assert_eq!(got, vec![b"early".to_vec(), b"late".to_vec()]);
    }

    #[test]
    fn oversized_datagrams_are_refused_not_fragmented() {
        // Fragmentation is a decision the layer above makes per channel. Hiding it here would let a
        // caller unknowingly send something the path cannot carry.
        let (mut a, _b) = loopback_pair();
        let big = vec![0u8; MTU + 1];
        assert!(matches!(
            a.send(B, &big, Timestamp::ZERO),
            Err(TransportError::PayloadTooLarge { len, mtu }) if len == MTU + 1 && mtu == MTU
        ));
        assert!(a.send(B, &vec![0u8; MTU], Timestamp::ZERO).is_ok());
    }

    #[test]
    fn in_flight_datagrams_are_observable() {
        let (mut a, mut b) = simulated_pair(LinkConditions::rtt(100, 0), 1);
        a.send(B, b"x", Timestamp::ZERO).unwrap();
        assert_eq!(a.network().in_flight(), 1);
        assert!(b.recv(Timestamp::from_millis(50)).is_some());
        assert_eq!(a.network().in_flight(), 0);
    }

    #[test]
    fn presets_are_ordered_by_severity() {
        assert!(LinkConditions::PERFECT.is_perfect());
        assert!(!LinkConditions::LAN.is_perfect());

        // Compared through a slice rather than pairwise: constant-folded comparisons are optimised
        // away and assert nothing at runtime.
        let ladder = [
            ("LAN", LinkConditions::LAN),
            ("BROADBAND", LinkConditions::BROADBAND),
            ("POOR_WIFI", LinkConditions::POOR_WIFI),
            ("MOBILE", LinkConditions::MOBILE),
        ];
        for pair in ladder.windows(2) {
            let (worse_name, worse) = pair[1];
            let (better_name, better) = pair[0];
            assert!(
                worse.latency_us > better.latency_us,
                "{worse_name} should have higher latency than {better_name}"
            );
            assert!(
                worse.loss_ppm >= better.loss_ppm,
                "{worse_name} should be at least as lossy as {better_name}"
            );
        }

        let r = LinkConditions::rtt(100, 10);
        assert_eq!(
            r.latency_us, 50_000,
            "one-way latency is half the round trip"
        );
        assert_eq!(r.loss_ppm, 100_000, "10 percent is 100,000 ppm");
    }
}
