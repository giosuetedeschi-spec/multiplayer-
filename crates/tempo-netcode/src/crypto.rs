//! Authenticated encryption and replay protection for packets.
//!
//! Every packet after the handshake is sealed with ChaCha20-Poly1305, so it cannot be forged, read
//! or modified in transit. The header travels in the clear and is authenticated as associated data,
//! which is what lets a receiver check the sequence before spending work on decryption.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

use crate::NetcodeError;

/// A 256-bit symmetric key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key(pub [u8; 32]);

impl Key {
    /// Generates a key from the operating system's entropy source.
    pub fn generate() -> Result<Key, NetcodeError> {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).map_err(|e| NetcodeError::Entropy(e.to_string()))?;
        Ok(Key(k))
    }

    /// Constructs from raw bytes.
    pub const fn from_bytes(b: [u8; 32]) -> Key {
        Key(b)
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// Keys must never reach a log, a panic message, or a bug report.
impl core::fmt::Debug for Key {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Key(<redacted>)")
    }
}

/// Bytes of authentication tag appended to every sealed payload.
pub const TAG_BYTES: usize = 16;

/// Builds the 96-bit packet nonce from a 64-bit sequence.
///
/// The sequence is unique for the life of a key, so the nonce is too. Session keys are ephemeral
/// and come from the connect token, so a sequence restarting at zero in a new session is safe —
/// but a sequence *repeating* under one key would be catastrophic, which is why the sequence is
/// extended to 64 bits rather than using the 16 bits the wire carries.
fn packet_nonce(sequence: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&sequence.to_le_bytes());
    *Nonce::from_slice(&n)
}

/// Encrypts and authenticates `plaintext`, binding it to `aad`.
pub fn seal(
    key: &Key,
    sequence: u64,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, NetcodeError> {
    let cipher = ChaCha20Poly1305::new(key.0.as_ref().into());
    cipher
        .encrypt(
            &packet_nonce(sequence),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| NetcodeError::Crypto)
}

/// Verifies and decrypts a payload produced by [`seal`].
///
/// Fails if the key, sequence or associated data differ by even one bit. The error is deliberately
/// undifferentiated — reporting *why* authentication failed would hand an attacker an oracle.
pub fn open(
    key: &Key,
    sequence: u64,
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, NetcodeError> {
    let cipher = ChaCha20Poly1305::new(key.0.as_ref().into());
    cipher
        .decrypt(
            &packet_nonce(sequence),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| NetcodeError::Crypto)
}

/// A sliding window that rejects replayed packet sequences.
///
/// Authentication alone does not stop replay: a captured packet is validly signed, so without this
/// an attacker could resend a "fire weapon" packet indefinitely.
#[derive(Debug, Clone)]
pub struct ReplayWindow {
    highest: u64,
    /// Bit *i* marks `highest - i` as seen.
    seen: u64,
    started: bool,
}

impl Default for ReplayWindow {
    fn default() -> ReplayWindow {
        ReplayWindow::new()
    }
}

impl ReplayWindow {
    /// Number of sequences behind the highest that are still tracked.
    pub const WIDTH: u64 = 64;

    /// An empty window.
    pub const fn new() -> ReplayWindow {
        ReplayWindow {
            highest: 0,
            seen: 0,
            started: false,
        }
    }

    /// Accepts a sequence, returning false if it is a replay or too old to judge.
    ///
    /// Sequences that fall behind the window are refused rather than accepted. A packet that late
    /// is useless to gameplay anyway, and accepting what cannot be checked defeats the purpose.
    pub fn accept(&mut self, sequence: u64) -> bool {
        if !self.started {
            self.started = true;
            self.highest = sequence;
            self.seen = 1;
            return true;
        }

        if sequence > self.highest {
            let shift = sequence - self.highest;
            self.seen = if shift >= Self::WIDTH {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.highest = sequence;
            return true;
        }

        let behind = self.highest - sequence;
        if behind >= Self::WIDTH {
            return false; // too old to judge
        }
        let mask = 1u64 << behind;
        if self.seen & mask != 0 {
            return false; // already seen
        }
        self.seen |= mask;
        true
    }

    /// The highest sequence accepted so far.
    #[inline]
    pub fn highest(&self) -> u64 {
        self.highest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key::from_bytes([7u8; 32])
    }

    #[test]
    fn sealed_payloads_round_trip() {
        let k = key();
        let sealed = seal(&k, 1, b"header", b"secret").unwrap();
        assert_ne!(
            sealed, b"secret",
            "the payload must not travel in the clear"
        );
        assert_eq!(sealed.len(), b"secret".len() + TAG_BYTES);
        assert_eq!(open(&k, 1, b"header", &sealed).unwrap(), b"secret");
    }

    #[test]
    fn tampering_is_detected_everywhere_it_could_happen() {
        let k = key();
        let sealed = seal(&k, 1, b"header", b"secret").unwrap();

        // Wrong key.
        assert!(open(&Key::from_bytes([8u8; 32]), 1, b"header", &sealed).is_err());
        // Wrong sequence: replaying a packet under a different number must not verify.
        assert!(open(&k, 2, b"header", &sealed).is_err());
        // Modified associated data: the header is in the clear but still authenticated.
        assert!(open(&k, 1, b"header!", &sealed).is_err());
        // Modified ciphertext.
        let mut corrupt = sealed.clone();
        corrupt[0] ^= 1;
        assert!(open(&k, 1, b"header", &corrupt).is_err());
        // Truncated payload.
        assert!(open(&k, 1, b"header", &sealed[..sealed.len() - 1]).is_err());
    }

    #[test]
    fn the_same_plaintext_encrypts_differently_under_different_sequences() {
        // Otherwise an observer could tell that a player sent the same input twice.
        let k = key();
        assert_ne!(
            seal(&k, 1, b"", b"same").unwrap(),
            seal(&k, 2, b"", b"same").unwrap()
        );
    }

    #[test]
    fn generated_keys_differ() {
        assert_ne!(Key::generate().unwrap(), Key::generate().unwrap());
    }

    #[test]
    fn keys_are_redacted_in_debug_output() {
        // A key reaching a log or a bug report is a compromise.
        let rendered = format!("{:?}", key());
        assert_eq!(rendered, "Key(<redacted>)");
        assert!(!rendered.contains('7'));
    }

    #[test]
    fn the_replay_window_accepts_each_sequence_once() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(0));
        assert!(!w.accept(0), "a replayed packet must be refused");
        assert!(w.accept(1));
        assert!(!w.accept(1));
    }

    #[test]
    fn the_replay_window_tolerates_reordering() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(10));
        // Out of order but within the window: legitimate on an unreliable transport.
        assert!(w.accept(7));
        assert!(w.accept(9));
        assert!(!w.accept(9), "still only once");
        assert!(w.accept(11));
        assert_eq!(w.highest(), 11);
    }

    #[test]
    fn sequences_behind_the_window_are_refused() {
        // Accepting what cannot be checked would defeat the purpose. A packet this late is useless
        // to gameplay anyway.
        let mut w = ReplayWindow::new();
        assert!(w.accept(0));
        assert!(w.accept(1000));
        assert!(!w.accept(1));
        assert!(!w.accept(1000 - ReplayWindow::WIDTH));
        assert!(w.accept(1000 - ReplayWindow::WIDTH + 1));
    }

    #[test]
    fn a_large_forward_jump_resets_the_window_cleanly() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(0));
        assert!(w.accept(1));
        // Jumping beyond the window width must not leave stale bits marking sequences as seen.
        assert!(w.accept(10_000));
        assert!(w.accept(9_999));
        assert!(!w.accept(10_000));
    }

    #[test]
    fn the_window_holds_a_full_burst() {
        let mut w = ReplayWindow::new();
        for seq in 0..ReplayWindow::WIDTH {
            assert!(w.accept(seq), "sequence {seq} should be new");
        }
        for seq in 0..ReplayWindow::WIDTH {
            assert!(!w.accept(seq), "sequence {seq} should be a replay");
        }
    }
}
