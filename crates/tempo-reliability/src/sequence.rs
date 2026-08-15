//! Wraparound-safe packet sequence numbers and the ring buffer keyed by them.
//!
//! Sequences are 16-bit and wrap roughly every 18 minutes at 60 packets per second, so wraparound
//! is an ordinary event rather than a theoretical one. Every comparison goes through
//! [`is_newer`]; using `<` directly inverts near the wrap point and produces a bug that appears
//! only after a session has been running for a while, which is the worst kind to debug.

/// True if `a` is more recent than `b`, accounting for wraparound.
///
/// Works by treating the difference as signed: sequences within half the space of each other
/// compare correctly regardless of where the wrap falls.
#[inline]
pub fn is_newer(a: u16, b: u16) -> bool {
    a.wrapping_sub(b) != 0 && a.wrapping_sub(b) < 0x8000
}

/// Signed distance from `b` to `a`, correct across wraparound.
#[inline]
pub fn distance(a: u16, b: u16) -> i32 {
    let d = a.wrapping_sub(b);
    if d < 0x8000 {
        d as i32
    } else {
        d as i32 - 0x10000
    }
}

/// Reconstructs a 64-bit sequence from a 16-bit wire value and the highest seen so far.
///
/// The wire carries only 16 bits, but the AEAD nonce needs a value that never repeats for the life
/// of a key. Both sides therefore extend the transmitted sequence using their own high bits,
/// choosing whichever candidate is closest to what they have already seen — the same technique
/// DTLS and QUIC use.
pub fn extend(wire: u16, highest_seen: u64) -> u64 {
    let base = highest_seen & !0xFFFF;
    let candidates = [
        base.wrapping_add(wire as u64),
        base.wrapping_add(0x1_0000).wrapping_add(wire as u64),
        base.wrapping_sub(0x1_0000).wrapping_add(wire as u64),
    ];
    let mut best = candidates[0];
    let mut best_gap = u64::MAX;
    for c in candidates {
        let gap = c.abs_diff(highest_seen);
        if gap < best_gap {
            best_gap = gap;
            best = c;
        }
    }
    best
}

/// A fixed-capacity ring indexed by sequence number.
///
/// Every read verifies the stored sequence before returning the entry. Without that check a slot
/// recycled `capacity` packets later would be served as the sequence that was asked for — a silent
/// wrong answer, which is worse than a missing one.
#[derive(Debug, Clone)]
pub struct SequenceBuffer<T> {
    entries: Vec<Option<(u16, T)>>,
}

impl<T> SequenceBuffer<T> {
    /// Creates a buffer holding `capacity` sequences of history.
    pub fn new(capacity: usize) -> SequenceBuffer<T> {
        assert!(capacity > 0, "a sequence buffer needs at least one slot");
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, || None);
        SequenceBuffer { entries }
    }

    /// Number of slots.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    fn index(&self, seq: u16) -> usize {
        seq as usize % self.entries.len()
    }

    /// Stores a value, replacing whatever occupied the slot.
    pub fn insert(&mut self, seq: u16, value: T) {
        let i = self.index(seq);
        self.entries[i] = Some((seq, value));
    }

    /// Reads a value, if this exact sequence is still stored.
    pub fn get(&self, seq: u16) -> Option<&T> {
        let i = self.index(seq);
        match &self.entries[i] {
            Some((s, v)) if *s == seq => Some(v),
            _ => None,
        }
    }

    /// Mutably reads a value, if this exact sequence is still stored.
    pub fn get_mut(&mut self, seq: u16) -> Option<&mut T> {
        let i = self.index(seq);
        match &mut self.entries[i] {
            Some((s, v)) if *s == seq => Some(v),
            _ => None,
        }
    }

    /// Removes and returns a value, if present.
    pub fn remove(&mut self, seq: u16) -> Option<T> {
        let i = self.index(seq);
        match &self.entries[i] {
            Some((s, _)) if *s == seq => self.entries[i].take().map(|(_, v)| v),
            _ => None,
        }
    }

    /// True if this exact sequence is stored.
    #[inline]
    pub fn contains(&self, seq: u16) -> bool {
        self.get(seq).is_some()
    }

    /// Empties the buffer.
    pub fn clear(&mut self) {
        for e in &mut self.entries {
            *e = None;
        }
    }

    /// Iterates stored `(sequence, value)` pairs in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &T)> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().map(|(s, v)| (*s, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_comparison_survives_wraparound() {
        assert!(is_newer(5, 4));
        assert!(!is_newer(4, 5));
        assert!(!is_newer(5, 5), "a sequence is not newer than itself");

        // The case a naive `<` gets wrong.
        assert!(is_newer(0, u16::MAX));
        assert!(is_newer(2, u16::MAX - 2));
        assert!(!is_newer(u16::MAX, 0));
    }

    #[test]
    fn distance_is_signed_and_wraps() {
        assert_eq!(distance(10, 4), 6);
        assert_eq!(distance(4, 10), -6);
        assert_eq!(distance(1, u16::MAX), 2);
        assert_eq!(distance(u16::MAX, 1), -2);
    }

    #[test]
    fn extension_picks_the_nearest_candidate() {
        // Ordinary case: no wrap.
        assert_eq!(extend(500, 400), 500);
        // Just wrapped: the wire value is small but the true sequence is in the next window.
        assert_eq!(extend(2, 0xFFFF), 0x1_0002);
        // Late arrival from just before the wrap.
        assert_eq!(extend(0xFFFE, 0x1_0002), 0xFFFE);
    }

    #[test]
    fn extension_never_repeats_a_nonce_across_a_wrap() {
        // The property that matters: extending a full cycle of wire sequences must produce
        // strictly increasing 64-bit values, because a repeated AEAD nonce is a security failure.
        let mut highest = 0u64;
        let mut last = None;
        for i in 0..200_000u32 {
            let wire = (i % 0x1_0000) as u16;
            let ext = extend(wire, highest);
            if let Some(prev) = last {
                assert_eq!(
                    ext,
                    prev + 1,
                    "extended sequence must advance by one at {i}"
                );
            }
            last = Some(ext);
            highest = ext;
        }
    }

    #[test]
    fn the_buffer_verifies_the_sequence_before_returning() {
        // A recycled slot must report absent, not serve a different sequence's value.
        let mut b: SequenceBuffer<u32> = SequenceBuffer::new(4);
        b.insert(0, 100);
        assert_eq!(b.get(0), Some(&100));

        b.insert(4, 200); // same slot as sequence 0
        assert_eq!(b.get(0), None, "sequence 0's slot now holds sequence 4");
        assert_eq!(b.get(4), Some(&200));
    }

    #[test]
    fn removal_and_mutation_respect_the_sequence_check() {
        let mut b: SequenceBuffer<u32> = SequenceBuffer::new(8);
        b.insert(3, 1);
        *b.get_mut(3).unwrap() = 2;
        assert_eq!(b.get(3), Some(&2));
        assert_eq!(b.remove(3), Some(2));
        assert_eq!(b.remove(3), None);
        assert!(!b.contains(3));

        b.insert(3, 9);
        assert_eq!(
            b.remove(11),
            None,
            "a recycled slot must not be removable by the wrong key"
        );
        assert_eq!(b.get(3), Some(&9));
    }

    #[test]
    fn the_buffer_works_across_the_wrap_point() {
        let mut b: SequenceBuffer<u16> = SequenceBuffer::new(64);
        for i in 0..200u32 {
            let seq = (u16::MAX - 100).wrapping_add(i as u16);
            b.insert(seq, seq);
            assert_eq!(b.get(seq), Some(&seq));
        }
    }
}
