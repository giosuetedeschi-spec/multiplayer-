//! Acknowledgement tracking and round-trip time estimation.
//!
//! Each packet header carries its own sequence, the highest sequence received from the peer, and a
//! 32-bit bitfield covering the 32 before that. One header therefore acknowledges up to 33 packets,
//! and because acks ride on every packet the redundancy makes them robust to loss without needing
//! reliability of their own.
//!
//! # Why this is not an external library
//!
//! Delta compression must know precisely which snapshot a client has acknowledged, in order to pick
//! the right baseline ([ADR-0009](../../../docs/adr/0009-custom-bitpacked-wire-format.md)). Adopting
//! a transport that owns its own acks would mean either duplicating this state or reaching through
//! an abstraction never designed to expose it. Here the replication layer reads
//! [`AckTracker::newly_acked`] directly, so baseline selection is exact rather than a guess.

use crate::sequence::{is_newer, SequenceBuffer};
use tempo_transport::Timestamp;

/// Number of prior packets covered by the ack bitfield.
pub const ACK_BITS: u32 = 32;

/// What a sent packet needs remembered about it until it is acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SentPacket {
    sent_at: Timestamp,
    acked: bool,
}

/// The acknowledgement half of a connection.
#[derive(Debug, Clone)]
pub struct AckTracker {
    /// Sequence to use for the next outgoing packet.
    next_sequence: u16,
    /// Highest sequence received from the peer, if any.
    remote_highest: Option<u16>,
    /// Which recent sequences arrived from the peer.
    received: SequenceBuffer<()>,
    /// Outgoing packets awaiting acknowledgement.
    sent: SequenceBuffer<SentPacket>,
    /// Sequences acknowledged since the last drain.
    newly_acked: Vec<u16>,
    rtt: RttEstimator,
}

impl AckTracker {
    /// Creates a tracker remembering `capacity` packets in each direction.
    ///
    /// Capacity must comfortably exceed the number of packets in flight over one round trip, or
    /// acknowledgements will arrive for packets already forgotten and be counted as loss.
    pub fn new(capacity: usize) -> AckTracker {
        AckTracker {
            next_sequence: 0,
            remote_highest: None,
            received: SequenceBuffer::new(capacity),
            sent: SequenceBuffer::new(capacity),
            newly_acked: Vec::new(),
            rtt: RttEstimator::new(),
        }
    }

    /// Allocates the sequence for the next outgoing packet and records it as in flight.
    pub fn begin_send(&mut self, now: Timestamp) -> u16 {
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.sent.insert(
            seq,
            SentPacket {
                sent_at: now,
                acked: false,
            },
        );
        seq
    }

    /// Records that a packet arrived from the peer.
    ///
    /// Ignores sequences too old to fit the buffer rather than corrupting newer entries.
    pub fn on_receive(&mut self, seq: u16) {
        match self.remote_highest {
            Some(h) if !is_newer(seq, h) => {}
            _ => self.remote_highest = Some(seq),
        }
        self.received.insert(seq, ());
    }

    /// The `(ack, ack_bits)` pair to place in an outgoing header.
    ///
    /// `ack` is the highest sequence received; bit *i* of `ack_bits` covers `ack - 1 - i`.
    pub fn ack_header(&self) -> (u16, u32) {
        let Some(ack) = self.remote_highest else {
            return (0, 0);
        };
        let mut bits = 0u32;
        for i in 0..ACK_BITS {
            let seq = ack.wrapping_sub(1).wrapping_sub(i as u16);
            if self.received.contains(seq) {
                bits |= 1 << i;
            }
        }
        (ack, bits)
    }

    /// Processes an incoming header's acknowledgements.
    ///
    /// Newly acknowledged sequences are appended to [`AckTracker::newly_acked`] and each produces
    /// an RTT sample.
    pub fn on_ack_header(&mut self, ack: u16, ack_bits: u32, now: Timestamp) {
        self.ack_one(ack, now);
        for i in 0..ACK_BITS {
            if ack_bits & (1 << i) != 0 {
                self.ack_one(ack.wrapping_sub(1).wrapping_sub(i as u16), now);
            }
        }
    }

    fn ack_one(&mut self, seq: u16, now: Timestamp) {
        let Some(entry) = self.sent.get_mut(seq) else {
            // Either already forgotten, or never sent. Both are ordinary under loss and reordering.
            return;
        };
        if entry.acked {
            // Acks are intentionally redundant, so the same sequence is confirmed many times. Only
            // the first is a new event, and only the first yields an RTT sample — sampling the
            // repeats would bias the estimate upward without adding information.
            return;
        }
        entry.acked = true;
        let sample = now.since(entry.sent_at);
        self.rtt.sample(sample);
        self.newly_acked.push(seq);
    }

    /// Takes the sequences acknowledged since the last call.
    pub fn drain_newly_acked(&mut self) -> Vec<u16> {
        core::mem::take(&mut self.newly_acked)
    }

    /// Sequences acknowledged since the last drain, without consuming them.
    pub fn newly_acked(&self) -> &[u16] {
        &self.newly_acked
    }

    /// True if this outgoing sequence has been acknowledged and is still remembered.
    pub fn is_acked(&self, seq: u16) -> bool {
        self.sent.get(seq).is_some_and(|p| p.acked)
    }

    /// Outgoing sequences sent before `now - timeout_us` that are still unacknowledged.
    pub fn timed_out(&self, now: Timestamp, timeout_us: u64) -> Vec<u16> {
        self.sent
            .iter()
            .filter(|(_, p)| !p.acked && now.since(p.sent_at) >= timeout_us)
            .map(|(s, _)| s)
            .collect()
    }

    /// The round-trip time estimator.
    #[inline]
    pub fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }

    /// The highest sequence received from the peer.
    #[inline]
    pub fn remote_highest(&self) -> Option<u16> {
        self.remote_highest
    }
}

/// Smoothed round-trip time, following the standard estimator from RFC 6298.
///
/// Tracks both a smoothed mean and a variance, because a retransmission timeout derived from the
/// mean alone fires spuriously on any jittery link — and a spurious retransmit under congestion is
/// exactly the wrong response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RttEstimator {
    srtt_us: u64,
    rttvar_us: u64,
    samples: u64,
    min_us: u64,
}

impl Default for RttEstimator {
    fn default() -> RttEstimator {
        RttEstimator::new()
    }
}

impl RttEstimator {
    /// A fresh estimator with no samples.
    pub const fn new() -> RttEstimator {
        RttEstimator {
            srtt_us: 0,
            rttvar_us: 0,
            samples: 0,
            min_us: u64::MAX,
        }
    }

    /// Incorporates a round-trip measurement, in microseconds.
    pub fn sample(&mut self, rtt_us: u64) {
        self.min_us = self.min_us.min(rtt_us);
        if self.samples == 0 {
            self.srtt_us = rtt_us;
            self.rttvar_us = rtt_us / 2;
        } else {
            // RFC 6298 with alpha = 1/8, beta = 1/4, in integer arithmetic.
            let delta = self.srtt_us.abs_diff(rtt_us);
            self.rttvar_us = (3 * self.rttvar_us + delta) / 4;
            self.srtt_us = (7 * self.srtt_us + rtt_us) / 8;
        }
        self.samples += 1;
    }

    /// Smoothed round-trip time, in microseconds.
    #[inline]
    pub fn srtt_us(&self) -> u64 {
        self.srtt_us
    }

    /// Round-trip time variation, in microseconds. A jitter estimate.
    #[inline]
    pub fn variation_us(&self) -> u64 {
        self.rttvar_us
    }

    /// Lowest round trip observed, which approximates the path's floor with no queueing.
    #[inline]
    pub fn min_us(&self) -> u64 {
        if self.samples == 0 {
            0
        } else {
            self.min_us
        }
    }

    /// Number of samples taken.
    #[inline]
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Retransmission timeout: `srtt + 4 * rttvar`, floored at 10 ms.
    ///
    /// The floor matters on a LAN, where an unfloored timeout would be tens of microseconds and
    /// every packet would be retransmitted before its acknowledgement could possibly return.
    pub fn rto_us(&self) -> u64 {
        if self.samples == 0 {
            return 200_000;
        }
        (self.srtt_us + 4 * self.rttvar_us).max(10_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn sequences_are_allocated_in_order() {
        let mut a = AckTracker::new(64);
        assert_eq!(a.begin_send(at(0)), 0);
        assert_eq!(a.begin_send(at(0)), 1);
        assert_eq!(a.begin_send(at(0)), 2);
    }

    #[test]
    fn the_header_reports_the_highest_and_a_bitfield_of_the_rest() {
        let mut a = AckTracker::new(64);
        for seq in [0u16, 1, 2, 4] {
            a.on_receive(seq);
        }
        let (ack, bits) = a.ack_header();
        assert_eq!(ack, 4);
        assert_eq!(bits & 1, 0, "sequence 3 was not received");
        assert_eq!(bits & 0b10, 0b10, "sequence 2 was");
        assert_eq!(bits & 0b100, 0b100, "sequence 1 was");
        assert_eq!(bits & 0b1000, 0b1000, "sequence 0 was");
    }

    #[test]
    fn out_of_order_arrival_does_not_lower_the_ack() {
        let mut a = AckTracker::new(64);
        a.on_receive(10);
        a.on_receive(7);
        let (ack, bits) = a.ack_header();
        assert_eq!(ack, 10, "a late packet must not move the ack backwards");
        assert_eq!(
            bits & (1 << 2),
            1 << 2,
            "but it is still acknowledged in the bitfield"
        );
    }

    #[test]
    fn a_single_header_acknowledges_many_packets() {
        let mut sender = AckTracker::new(64);
        for _ in 0..5 {
            sender.begin_send(at(0));
        }
        let mut receiver = AckTracker::new(64);
        for seq in 0..5u16 {
            receiver.on_receive(seq);
        }
        let (ack, bits) = receiver.ack_header();
        sender.on_ack_header(ack, bits, at(50));

        let acked = sender.drain_newly_acked();
        assert_eq!(acked.len(), 5, "one header confirmed all five");
        for seq in 0..5u16 {
            assert!(sender.is_acked(seq));
        }
    }

    #[test]
    fn redundant_acks_are_reported_once_and_sampled_once() {
        // Acks repeat on every packet by design. Counting the repeats would bias the RTT estimate
        // upward, since later confirmations look like slower round trips.
        let mut sender = AckTracker::new(64);
        sender.begin_send(at(0));

        sender.on_ack_header(0, 0, at(40));
        assert_eq!(sender.drain_newly_acked(), vec![0]);
        let rtt_after_first = sender.rtt().srtt_us();

        sender.on_ack_header(0, 0, at(500));
        assert!(
            sender.drain_newly_acked().is_empty(),
            "already acknowledged"
        );
        assert_eq!(sender.rtt().srtt_us(), rtt_after_first, "no second sample");
        assert_eq!(sender.rtt().samples(), 1);
    }

    #[test]
    fn loss_leaves_gaps_that_time_out() {
        let mut sender = AckTracker::new(64);
        for _ in 0..3 {
            sender.begin_send(at(0));
        }
        // Only sequence 2 arrives.
        let mut receiver = AckTracker::new(64);
        receiver.on_receive(2);
        let (ack, bits) = receiver.ack_header();
        sender.on_ack_header(ack, bits, at(30));

        assert_eq!(sender.drain_newly_acked(), vec![2]);
        let mut stale = sender.timed_out(at(500), 100_000);
        stale.sort_unstable();
        assert_eq!(stale, vec![0, 1]);
    }

    #[test]
    fn acknowledgement_works_across_the_sequence_wrap() {
        let mut sender = AckTracker::new(64);
        // Fast-forward the sequence counter to just before the wrap.
        for _ in 0..u16::MAX {
            sender.begin_send(at(0));
        }
        let a = sender.begin_send(at(0)); // 0xFFFF
        let b = sender.begin_send(at(0)); // wraps to 0
        assert_eq!((a, b), (0xFFFF, 0));

        let mut receiver = AckTracker::new(64);
        receiver.on_receive(a);
        receiver.on_receive(b);
        let (ack, bits) = receiver.ack_header();
        assert_eq!(ack, 0, "0 is newer than 0xFFFF");

        sender.on_ack_header(ack, bits, at(20));
        let mut acked = sender.drain_newly_acked();
        acked.sort_unstable();
        assert_eq!(acked, vec![0, 0xFFFF]);
    }

    #[test]
    fn acks_for_forgotten_packets_are_ignored() {
        let mut sender = AckTracker::new(8);
        for _ in 0..40 {
            sender.begin_send(at(0));
        }
        // Sequence 0 has long since been evicted from an 8-slot buffer.
        sender.on_ack_header(0, 0, at(10));
        assert!(sender.drain_newly_acked().is_empty());
    }

    #[test]
    fn rtt_converges_and_reports_variation() {
        let mut r = RttEstimator::new();
        assert_eq!(
            r.rto_us(),
            200_000,
            "no samples yet, so a conservative default"
        );

        for _ in 0..50 {
            r.sample(40_000);
        }
        assert!(
            (39_000..=41_000).contains(&r.srtt_us()),
            "srtt was {}",
            r.srtt_us()
        );
        assert!(
            r.variation_us() < 2_000,
            "a steady link should show little variation"
        );
        assert_eq!(r.min_us(), 40_000);

        // A jittery link must widen the timeout rather than retransmit spuriously.
        let steady_rto = r.rto_us();
        for i in 0..50 {
            r.sample(if i % 2 == 0 { 10_000 } else { 90_000 });
        }
        assert!(
            r.rto_us() > steady_rto,
            "jitter must widen the retransmission timeout"
        );
    }

    #[test]
    fn the_retransmission_timeout_has_a_floor() {
        // On a LAN an unfloored timeout would be tens of microseconds, and every packet would be
        // retransmitted before its acknowledgement could return.
        let mut r = RttEstimator::new();
        for _ in 0..100 {
            r.sample(50);
        }
        assert_eq!(r.rto_us(), 10_000);
    }
}
