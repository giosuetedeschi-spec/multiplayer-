//! Bit-granular reader and writer.
//!
//! Bits are packed **least-significant-bit first**: the first bit written occupies bit 0 of byte 0,
//! the ninth occupies bit 0 of byte 1. A value of `n` bits is written least significant bit first.
//! See `docs/spec/wire-protocol.md` §1 — this is normative and is the single most common source of
//! cross-implementation disagreement, because it is not the convention most people assume.

use crate::WireError;

/// Writes values at bit granularity into a growable byte buffer.
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Pending bits, held at the low end. Always fewer than 8 between calls.
    acc: u64,
    acc_bits: u32,
}

impl BitWriter {
    /// Creates an empty writer.
    #[inline]
    pub fn new() -> BitWriter {
        BitWriter::default()
    }

    /// Creates a writer with room for `cap` bytes reserved up front.
    #[inline]
    pub fn with_capacity(cap: usize) -> BitWriter {
        BitWriter {
            bytes: Vec::with_capacity(cap),
            acc: 0,
            acc_bits: 0,
        }
    }

    /// Number of bits written so far, including any not yet flushed to a whole byte.
    #[inline]
    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8 + self.acc_bits as usize
    }

    /// Writes the low `bits` bits of `value`.
    ///
    /// Bits above `bits` are ignored rather than causing an error, so a caller that has already
    /// range-checked its value does not pay for a second check.
    pub fn write_bits(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 64, "cannot write more than 64 bits at once");
        if bits == 0 {
            return;
        }
        if bits > 32 {
            // Split so the mask below never has to shift by 64, which is undefined.
            self.write_bits(value & 0xFFFF_FFFF, 32);
            self.write_bits(value >> 32, bits - 32);
            return;
        }
        let masked = value & ((1u64 << bits) - 1);
        self.acc |= masked << self.acc_bits;
        self.acc_bits += bits;
        while self.acc_bits >= 8 {
            self.bytes.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.acc_bits -= 8;
        }
    }

    /// Writes a single bit.
    #[inline]
    pub fn write_bool(&mut self, v: bool) {
        self.write_bits(v as u64, 1);
    }

    /// Writes an unsigned LEB128-style varint at bit granularity.
    ///
    /// Each group is a continuation bit followed by seven payload bits, least significant group
    /// first.
    pub fn write_varuint(&mut self, mut v: u64) {
        loop {
            let payload = v & 0x7F;
            v >>= 7;
            let more = v != 0;
            self.write_bool(more);
            self.write_bits(payload, 7);
            if !more {
                return;
            }
        }
    }

    /// Writes a signed varint using zig-zag encoding.
    #[inline]
    pub fn write_varint(&mut self, v: i64) {
        self.write_varuint(zigzag_encode(v));
    }

    /// Pads with zero bits to the next byte boundary.
    #[inline]
    pub fn align(&mut self) {
        if self.acc_bits > 0 {
            self.write_bits(0, 8 - self.acc_bits);
        }
    }

    /// Writes raw bytes, aligning first.
    pub fn write_bytes(&mut self, data: &[u8]) {
        self.align();
        self.bytes.extend_from_slice(data);
    }

    /// Finishes writing and returns the buffer, zero-padded to a whole number of bytes.
    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.bytes
    }

    /// Returns the bytes written so far without consuming the writer, padding a partial byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.bytes.clone();
        if self.acc_bits > 0 {
            out.push((self.acc & 0xFF) as u8);
        }
        out
    }
}

/// Reads values at bit granularity from a byte slice.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Absolute bit position of the next bit to read.
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// Creates a reader over `bytes`.
    #[inline]
    pub fn new(bytes: &'a [u8]) -> BitReader<'a> {
        BitReader { bytes, pos: 0 }
    }

    /// Total number of readable bits.
    #[inline]
    pub fn bit_capacity(&self) -> usize {
        self.bytes.len() * 8
    }

    /// Number of bits consumed so far.
    #[inline]
    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Number of bits remaining.
    #[inline]
    pub fn bits_remaining(&self) -> usize {
        self.bit_capacity().saturating_sub(self.pos)
    }

    /// Reads `bits` bits, returning them in the low bits of a `u64`.
    pub fn read_bits(&mut self, bits: u32) -> Result<u64, WireError> {
        debug_assert!(bits <= 64);
        if bits == 0 {
            return Ok(0);
        }
        if bits > 32 {
            let lo = self.read_bits(32)?;
            let hi = self.read_bits(bits - 32)?;
            return Ok(lo | (hi << 32));
        }
        if self.bits_remaining() < bits as usize {
            return Err(WireError::UnexpectedEnd);
        }
        let mut out: u64 = 0;
        let mut taken = 0u32;
        while taken < bits {
            let byte = self.bytes[self.pos / 8];
            let bit_in_byte = (self.pos % 8) as u32;
            let available = 8 - bit_in_byte;
            let want = (bits - taken).min(available);
            let chunk = ((byte >> bit_in_byte) as u64) & ((1u64 << want) - 1);
            out |= chunk << taken;
            taken += want;
            self.pos += want as usize;
        }
        Ok(out)
    }

    /// Reads a single bit.
    #[inline]
    pub fn read_bool(&mut self) -> Result<bool, WireError> {
        Ok(self.read_bits(1)? != 0)
    }

    /// Reads an unsigned varint written by [`BitWriter::write_varuint`].
    pub fn read_varuint(&mut self) -> Result<u64, WireError> {
        let mut out: u64 = 0;
        let mut shift = 0u32;
        loop {
            let more = self.read_bool()?;
            let payload = self.read_bits(7)?;
            if shift >= 64 {
                // A well-formed varint never needs a tenth group. Refusing rather than wrapping
                // keeps a malformed packet from silently decoding to a plausible value.
                return Err(WireError::MalformedVarint);
            }
            out |= payload << shift;
            shift += 7;
            if !more {
                return Ok(out);
            }
        }
    }

    /// Reads a zig-zag encoded signed varint.
    #[inline]
    pub fn read_varint(&mut self) -> Result<i64, WireError> {
        Ok(zigzag_decode(self.read_varuint()?))
    }

    /// Skips forward to the next byte boundary.
    #[inline]
    pub fn align(&mut self) {
        let rem = self.pos % 8;
        if rem != 0 {
            self.pos += 8 - rem;
        }
    }

    /// Reads `len` raw bytes, aligning first.
    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        self.align();
        let start = self.pos / 8;
        let end = start.checked_add(len).ok_or(WireError::UnexpectedEnd)?;
        if end > self.bytes.len() {
            return Err(WireError::UnexpectedEnd);
        }
        self.pos = end * 8;
        Ok(&self.bytes[start..end])
    }
}

/// Maps a signed value onto an unsigned one so that small magnitudes stay small.
#[inline]
pub const fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// Inverse of [`zigzag_encode`].
#[inline]
pub const fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_across_byte_boundaries() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_bits(0xDEAD_BEEF, 32);
        w.write_bool(true);
        w.write_bits(0x7F, 7);
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.read_bits(32).unwrap(), 0xDEAD_BEEF);
        assert!(r.read_bool().unwrap());
        assert_eq!(r.read_bits(7).unwrap(), 0x7F);
    }

    #[test]
    fn first_bit_occupies_the_lowest_bit_of_the_first_byte() {
        // Pins the bit order itself, not just round-tripping. A big-endian implementation would
        // round-trip perfectly against itself and still be wrong on the wire.
        let mut w = BitWriter::new();
        w.write_bool(true);
        assert_eq!(w.to_bytes()[0] & 1, 1);

        let mut w = BitWriter::new();
        w.write_bits(0b10, 2);
        assert_eq!(w.to_bytes()[0], 0b10);
    }

    #[test]
    fn sixty_four_bit_values_survive_the_internal_split() {
        let mut w = BitWriter::new();
        w.write_bits(u64::MAX, 64);
        w.write_bits(0x1234_5678_9ABC_DEF0, 64);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(64).unwrap(), u64::MAX);
        assert_eq!(r.read_bits(64).unwrap(), 0x1234_5678_9ABC_DEF0);
    }

    #[test]
    fn varuint_round_trips_at_every_group_boundary() {
        let cases = [
            0u64,
            1,
            126,
            127,
            128,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX / 2,
            u64::MAX,
        ];
        for v in cases {
            let mut w = BitWriter::new();
            w.write_varuint(v);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_varuint().unwrap(), v, "varuint {v}");
        }
    }

    #[test]
    fn varint_round_trips_including_extremes() {
        for v in [0i64, -1, 1, -64, 63, i32::MIN as i64, i64::MIN, i64::MAX] {
            let mut w = BitWriter::new();
            w.write_varint(v);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_varint().unwrap(), v, "varint {v}");
        }
    }

    #[test]
    fn small_values_are_cheap() {
        // The whole point of varints here: a value under 128 must cost one 8-bit group.
        let mut w = BitWriter::new();
        w.write_varuint(5);
        assert_eq!(w.bit_len(), 8);
    }

    #[test]
    fn reading_past_the_end_errors_rather_than_panicking() {
        let bytes = [0xFFu8];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(8).unwrap(), 0xFF);
        assert!(matches!(r.read_bits(1), Err(WireError::UnexpectedEnd)));
    }

    #[test]
    fn malformed_varints_are_rejected_not_wrapped() {
        // Ten continuation groups is more than any u64 needs. A hostile peer must not be able to
        // make the decoder silently produce a plausible number.
        let mut w = BitWriter::new();
        for _ in 0..10 {
            w.write_bool(true);
            w.write_bits(0x7F, 7);
        }
        w.write_bool(false);
        w.write_bits(1, 7);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(matches!(r.read_varuint(), Err(WireError::MalformedVarint)));
    }

    #[test]
    fn zigzag_keeps_small_magnitudes_small() {
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(i64::MIN), u64::MAX);
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -12345, 12345] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v);
        }
    }

    #[test]
    fn byte_payloads_align_first() {
        let mut w = BitWriter::new();
        w.write_bits(0b1, 1);
        w.write_bytes(&[0xAA, 0xBB]);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(1).unwrap(), 1);
        assert_eq!(r.read_bytes(2).unwrap(), &[0xAA, 0xBB]);
    }
}
