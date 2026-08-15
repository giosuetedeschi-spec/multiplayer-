//! Fragmentation and reassembly for messages larger than the MTU.
//!
//! Implements `docs/spec/wire-protocol.md` §7. Snapshots are never fragmented — if a snapshot does
//! not fit, the priority accumulator sends less this tick, which is what it is for. Fragmentation
//! exists for large reliable messages: initial state transfer, schema diffs, host migration
//! payloads.
//!
//! # Every limit here is a denial-of-service bound
//!
//! A reassembler accepts attacker-controlled input by definition. Each of the following is a
//! guard rather than a tuning knob, and each is validated **before** any allocation:
//!
//! - fragment count, checked against a maximum before a buffer is sized
//! - total in-flight messages and total bytes
//! - a timeout, so an attacker cannot pin memory by never completing a message
//!
//! `Vec::with_capacity` on an attacker-chosen number is the textbook way to be exhausted, so the
//! count is checked first and the buffer grows as fragments actually arrive.

use std::collections::HashMap;

use tempo_transport::Timestamp;

/// Header prefixed to each fragment of a split message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    /// Identifies the message being reassembled.
    pub message_id: u32,
    /// This fragment's position, `0..count`.
    pub index: u16,
    /// Total fragments in the message.
    pub count: u16,
}

/// Limits applied to reassembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyLimits {
    /// Largest number of fragments one message may be split into.
    pub max_fragments: u16,
    /// Largest number of partially reassembled messages held at once.
    pub max_messages: usize,
    /// Largest total bytes held across all partial reassemblies.
    pub max_total_bytes: usize,
    /// How long an incomplete message is kept before being discarded, in microseconds.
    pub timeout_us: u64,
}

impl Default for ReassemblyLimits {
    fn default() -> ReassemblyLimits {
        ReassemblyLimits {
            // 256 fragments at ~1100 usable bytes is a ~280 KB message, ample for state transfer.
            max_fragments: 256,
            max_messages: 16,
            max_total_bytes: 4 * 1024 * 1024,
            timeout_us: 2_000_000,
        }
    }
}

/// Why a fragment was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentError {
    /// `count` was zero, or exceeded the configured maximum.
    BadFragmentCount(u16),
    /// `index` was at or beyond `count`.
    IndexOutOfRange {
        /// The index supplied.
        index: u16,
        /// The declared count.
        count: u16,
    },
    /// A fragment disagreed with earlier fragments about the message's shape.
    Inconsistent,
    /// Accepting this fragment would exceed a reassembly limit.
    LimitExceeded,
}

/// Splits `payload` into fragments of at most `max_fragment_bytes` each.
pub fn fragment(
    message_id: u32,
    payload: &[u8],
    max_fragment_bytes: usize,
) -> Vec<(FragmentHeader, Vec<u8>)> {
    assert!(max_fragment_bytes > 0, "fragment size must be positive");
    if payload.is_empty() {
        return vec![(
            FragmentHeader {
                message_id,
                index: 0,
                count: 1,
            },
            Vec::new(),
        )];
    }
    let count = payload.len().div_ceil(max_fragment_bytes);
    payload
        .chunks(max_fragment_bytes)
        .enumerate()
        .map(|(i, chunk)| {
            (
                FragmentHeader {
                    message_id,
                    index: i as u16,
                    count: count as u16,
                },
                chunk.to_vec(),
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Partial {
    count: u16,
    parts: Vec<Option<Vec<u8>>>,
    received: u16,
    bytes: usize,
    first_seen: Timestamp,
}

/// Reassembles fragmented messages, under bounded memory.
#[derive(Debug, Clone)]
pub struct Reassembler {
    limits: ReassemblyLimits,
    partial: HashMap<u32, Partial>,
    total_bytes: usize,
}

impl Default for Reassembler {
    fn default() -> Reassembler {
        Reassembler::new(ReassemblyLimits::default())
    }
}

impl Reassembler {
    /// Creates a reassembler with the given limits.
    pub fn new(limits: ReassemblyLimits) -> Reassembler {
        Reassembler {
            limits,
            partial: HashMap::new(),
            total_bytes: 0,
        }
    }

    /// Number of messages partially reassembled.
    #[inline]
    pub fn pending_messages(&self) -> usize {
        self.partial.len()
    }

    /// Total bytes currently held in partial reassemblies.
    #[inline]
    pub fn pending_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Accepts a fragment, returning the complete payload once the last one arrives.
    pub fn push(
        &mut self,
        header: FragmentHeader,
        data: &[u8],
        now: Timestamp,
    ) -> Result<Option<Vec<u8>>, FragmentError> {
        // Validate before allocating anything. This ordering is the point of the whole module.
        if header.count == 0 || header.count > self.limits.max_fragments {
            return Err(FragmentError::BadFragmentCount(header.count));
        }
        if header.index >= header.count {
            return Err(FragmentError::IndexOutOfRange {
                index: header.index,
                count: header.count,
            });
        }

        self.expire(now);

        let is_new = !self.partial.contains_key(&header.message_id);
        if is_new {
            if self.partial.len() >= self.limits.max_messages {
                return Err(FragmentError::LimitExceeded);
            }
            if self.total_bytes + data.len() > self.limits.max_total_bytes {
                return Err(FragmentError::LimitExceeded);
            }
            // Sized by the declared count, which has already been bounded above.
            let mut parts = Vec::new();
            parts.resize_with(header.count as usize, || None);
            self.partial.insert(
                header.message_id,
                Partial {
                    count: header.count,
                    parts,
                    received: 0,
                    bytes: 0,
                    first_seen: now,
                },
            );
        }

        let limit = self.limits.max_total_bytes;
        let total = self.total_bytes;
        let entry = self
            .partial
            .get_mut(&header.message_id)
            .expect("just inserted or present");

        if entry.count != header.count {
            // Two fragments disagreeing about the message shape means either corruption or an
            // attempt to confuse the reassembler. Drop the whole message rather than guess.
            let bytes = entry.bytes;
            self.partial.remove(&header.message_id);
            self.total_bytes -= bytes;
            return Err(FragmentError::Inconsistent);
        }
        if entry.parts[header.index as usize].is_some() {
            return Ok(None); // duplicate fragment
        }
        if total + data.len() > limit {
            return Err(FragmentError::LimitExceeded);
        }

        entry.parts[header.index as usize] = Some(data.to_vec());
        entry.received += 1;
        entry.bytes += data.len();
        self.total_bytes += data.len();

        if entry.received != entry.count {
            return Ok(None);
        }

        let done = self.partial.remove(&header.message_id).expect("present");
        self.total_bytes -= done.bytes;
        let mut out = Vec::with_capacity(done.bytes);
        for part in done.parts {
            out.extend_from_slice(&part.expect("all fragments present"));
        }
        Ok(Some(out))
    }

    /// Discards partial messages older than the configured timeout.
    ///
    /// Called automatically on every push, so an attacker cannot pin memory simply by going quiet.
    pub fn expire(&mut self, now: Timestamp) {
        let timeout = self.limits.timeout_us;
        let mut freed = 0usize;
        self.partial.retain(|_, p| {
            let keep = now.since(p.first_seen) < timeout;
            if !keep {
                freed += p.bytes;
            }
            keep
        });
        self.total_bytes -= freed;
    }

    /// Discards everything in progress.
    pub fn clear(&mut self) {
        self.partial.clear();
        self.total_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn a_message_round_trips_through_fragmentation() {
        let payload: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let parts = fragment(7, &payload, 1100);
        assert_eq!(parts.len(), 5);

        let mut r = Reassembler::default();
        let mut result = None;
        for (h, d) in &parts {
            result = r.push(*h, d, at(0)).unwrap();
        }
        assert_eq!(result, Some(payload));
        assert_eq!(r.pending_messages(), 0);
        assert_eq!(r.pending_bytes(), 0);
    }

    #[test]
    fn fragments_reassemble_out_of_order() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
        let mut parts = fragment(1, &payload, 1000);
        parts.reverse();

        let mut r = Reassembler::default();
        let mut result = None;
        for (h, d) in &parts {
            result = r.push(*h, d, at(0)).unwrap();
        }
        assert_eq!(result, Some(payload));
    }

    #[test]
    fn duplicate_fragments_are_ignored() {
        let payload = vec![9u8; 2500];
        let parts = fragment(2, &payload, 1000);

        let mut r = Reassembler::default();
        assert_eq!(r.push(parts[0].0, &parts[0].1, at(0)).unwrap(), None);
        assert_eq!(
            r.push(parts[0].0, &parts[0].1, at(0)).unwrap(),
            None,
            "duplicate"
        );
        assert_eq!(r.push(parts[1].0, &parts[1].1, at(0)).unwrap(), None);
        assert_eq!(
            r.push(parts[2].0, &parts[2].1, at(0)).unwrap(),
            Some(payload)
        );
    }

    #[test]
    fn an_empty_payload_is_a_single_fragment() {
        let parts = fragment(3, &[], 1000);
        assert_eq!(parts.len(), 1);
        let mut r = Reassembler::default();
        assert_eq!(
            r.push(parts[0].0, &parts[0].1, at(0)).unwrap(),
            Some(Vec::new())
        );
    }

    #[test]
    fn an_absurd_fragment_count_is_refused_before_allocating() {
        // The textbook exhaustion vector: sizing a buffer from an attacker-chosen number.
        let mut r = Reassembler::default();
        let h = FragmentHeader {
            message_id: 0,
            index: 0,
            count: u16::MAX,
        };
        assert_eq!(
            r.push(h, b"x", at(0)),
            Err(FragmentError::BadFragmentCount(u16::MAX))
        );
        assert_eq!(r.pending_messages(), 0, "nothing was allocated");
    }

    #[test]
    fn a_zero_fragment_count_is_refused() {
        let mut r = Reassembler::default();
        let h = FragmentHeader {
            message_id: 0,
            index: 0,
            count: 0,
        };
        assert_eq!(
            r.push(h, b"x", at(0)),
            Err(FragmentError::BadFragmentCount(0))
        );
    }

    #[test]
    fn an_index_beyond_the_count_is_refused() {
        let mut r = Reassembler::default();
        let h = FragmentHeader {
            message_id: 0,
            index: 5,
            count: 3,
        };
        assert_eq!(
            r.push(h, b"x", at(0)),
            Err(FragmentError::IndexOutOfRange { index: 5, count: 3 })
        );
    }

    #[test]
    fn disagreeing_fragments_drop_the_whole_message() {
        let mut r = Reassembler::default();
        r.push(
            FragmentHeader {
                message_id: 1,
                index: 0,
                count: 4,
            },
            b"a",
            at(0),
        )
        .unwrap();
        assert_eq!(
            r.push(
                FragmentHeader {
                    message_id: 1,
                    index: 1,
                    count: 9
                },
                b"b",
                at(0)
            ),
            Err(FragmentError::Inconsistent)
        );
        assert_eq!(
            r.pending_messages(),
            0,
            "the message is abandoned, not partially trusted"
        );
        assert_eq!(r.pending_bytes(), 0);
    }

    #[test]
    fn too_many_concurrent_messages_are_refused() {
        let limits = ReassemblyLimits {
            max_messages: 2,
            ..Default::default()
        };
        let mut r = Reassembler::new(limits);
        for id in 0..2u32 {
            r.push(
                FragmentHeader {
                    message_id: id,
                    index: 0,
                    count: 4,
                },
                b"x",
                at(0),
            )
            .unwrap();
        }
        assert_eq!(
            r.push(
                FragmentHeader {
                    message_id: 99,
                    index: 0,
                    count: 4
                },
                b"x",
                at(0)
            ),
            Err(FragmentError::LimitExceeded)
        );
    }

    #[test]
    fn total_bytes_are_bounded() {
        let limits = ReassemblyLimits {
            max_total_bytes: 100,
            ..Default::default()
        };
        let mut r = Reassembler::new(limits);
        r.push(
            FragmentHeader {
                message_id: 0,
                index: 0,
                count: 4,
            },
            &[0u8; 80],
            at(0),
        )
        .unwrap();
        assert_eq!(
            r.push(
                FragmentHeader {
                    message_id: 0,
                    index: 1,
                    count: 4
                },
                &[0u8; 80],
                at(0)
            ),
            Err(FragmentError::LimitExceeded)
        );
    }

    #[test]
    fn incomplete_messages_expire() {
        // Without this an attacker pins memory simply by never finishing a message.
        let limits = ReassemblyLimits {
            timeout_us: 1_000_000,
            ..Default::default()
        };
        let mut r = Reassembler::new(limits);
        r.push(
            FragmentHeader {
                message_id: 0,
                index: 0,
                count: 4,
            },
            &vec![0u8; 500],
            at(0),
        )
        .unwrap();
        assert_eq!(r.pending_messages(), 1);

        r.expire(at(2000));
        assert_eq!(r.pending_messages(), 0);
        assert_eq!(
            r.pending_bytes(),
            0,
            "expiry must release the accounted bytes too"
        );
    }

    #[test]
    fn expiry_runs_automatically_on_push() {
        let limits = ReassemblyLimits {
            timeout_us: 1_000_000,
            max_messages: 1,
            ..Default::default()
        };
        let mut r = Reassembler::new(limits);
        r.push(
            FragmentHeader {
                message_id: 0,
                index: 0,
                count: 4,
            },
            b"x",
            at(0),
        )
        .unwrap();
        // The slot is occupied, but the occupant has expired by the time the next one arrives.
        r.push(
            FragmentHeader {
                message_id: 1,
                index: 0,
                count: 4,
            },
            b"y",
            at(5000),
        )
        .expect("the stale message should have been evicted");
    }
}
