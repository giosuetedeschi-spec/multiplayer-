//! Transport abstraction: unreliable datagrams between peers.
//!
//! Everything above this layer — reliability, replication, prediction — is transport-agnostic. The
//! interface is deliberately the lowest common denominator of every transport in
//! [ADR-0005](../../../docs/adr/0005-transport-matrix.md): **unreliable, unordered datagrams with a
//! bounded MTU**. QUIC's reliable streams and WebRTC's ordered mode are not exposed, because
//! building on them would mean transport-specific code paths above this line.
//!
//! # Time is injected, never read
//!
//! No transport here calls `Instant::now()`. Every method that needs the current time takes a
//! [`Timestamp`]. This is not a testing convenience bolted on afterwards — it is what makes the
//! simulated link reproducible, and it is the same property that lets a recorded session replay
//! exactly ([ADR-0020](../../../docs/adr/0020-observability.md)). A transport that read the clock
//! itself could not be replayed.
//!
//! # What is here
//!
//! - [`MemoryTransport`] — an in-memory network. With default conditions it is a perfect loopback
//!   for tests, single-player and local co-op. With [`LinkConditions`] it becomes a *simulated
//!   link*: latency, jitter, loss, duplication and reordering, driven by a seeded PRNG.
//! - [`UdpTransport`] — real UDP sockets.
//!
//! The simulated link is arguably the most valuable thing in this crate. Prediction, reconciliation
//! and rollback only reveal their bugs under latency and loss, and those are exactly the conditions
//! that are hardest to reproduce against a real network. Here they are a config struct and a seed.

#![forbid(unsafe_code)]

pub mod memory;
pub mod rng;
pub mod udp;

pub use memory::{LinkConditions, MemoryNetwork, MemoryTransport};
pub use rng::Rng;
pub use udp::UdpTransport;

use core::fmt;

/// Maximum datagram payload, in bytes.
///
/// Deliberately conservative — below the smallest path MTU we expect on the public internet — so
/// that fragmentation is ours to control rather than IP's. See `docs/spec/wire-protocol.md` §1.
pub const MTU: usize = 1200;

/// Identifies a peer within a transport.
///
/// This is a transport-level address, not a game-level identity. Mapping it to an authenticated
/// player is the connection layer's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub u64);

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peer#{}", self.0)
    }
}

/// Microseconds since an arbitrary session-local epoch.
///
/// Microseconds rather than milliseconds because jitter and one-way latency are routinely measured
/// in fractions of a millisecond, and rounding them away would make the simulated link coarser than
/// the effects it exists to reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// The session epoch.
    pub const ZERO: Timestamp = Timestamp(0);

    /// Constructs from milliseconds.
    #[inline]
    pub const fn from_millis(ms: u64) -> Timestamp {
        Timestamp(ms.saturating_mul(1_000))
    }

    /// Constructs from microseconds.
    #[inline]
    pub const fn from_micros(us: u64) -> Timestamp {
        Timestamp(us)
    }

    /// Whole milliseconds since the epoch.
    #[inline]
    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000
    }

    /// Microseconds since the epoch.
    #[inline]
    pub const fn as_micros(self) -> u64 {
        self.0
    }

    /// This timestamp advanced by `us` microseconds, saturating.
    #[inline]
    pub const fn plus_micros(self, us: u64) -> Timestamp {
        Timestamp(self.0.saturating_add(us))
    }

    /// This timestamp advanced by `ms` milliseconds, saturating.
    #[inline]
    pub const fn plus_millis(self, ms: u64) -> Timestamp {
        self.plus_micros(ms.saturating_mul(1_000))
    }

    /// Microseconds elapsed since `earlier`, saturating at zero.
    #[inline]
    pub const fn since(self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// A datagram that arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// Who sent it.
    pub from: PeerId,
    /// The payload.
    pub data: Vec<u8>,
}

/// Errors a transport can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The payload exceeded [`MTU`].
    ///
    /// Not silently fragmented: fragmentation is a decision the layer above makes, per channel, and
    /// hiding it here would let a caller unknowingly send a datagram that cannot survive the path.
    PayloadTooLarge {
        /// Size attempted.
        len: usize,
        /// The limit.
        mtu: usize,
    },
    /// The destination is not known to this transport.
    UnknownPeer(PeerId),
    /// The underlying socket or channel failed.
    Io(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::PayloadTooLarge { len, mtu } => {
                write!(f, "datagram of {len} bytes exceeds the {mtu} byte MTU")
            }
            TransportError::UnknownPeer(p) => write!(f, "unknown destination {p}"),
            TransportError::Io(e) => write!(f, "transport io: {e}"),
        }
    }
}

impl core::error::Error for TransportError {}

/// An unreliable datagram transport.
///
/// Implementations make no delivery, ordering or duplication guarantees. That is not a limitation
/// to be worked around — it is the contract the reliability layer is written against, and a
/// transport that quietly delivered everything in order would let bugs hide until production.
pub trait Transport {
    /// Queues a datagram for delivery.
    ///
    /// Returning `Ok` means the datagram was accepted, never that it arrived.
    fn send(&mut self, to: PeerId, data: &[u8], now: Timestamp) -> Result<(), TransportError>;

    /// Takes the next datagram that has arrived by `now`, if any.
    ///
    /// Callers drain in a loop until this returns `None`.
    fn recv(&mut self, now: Timestamp) -> Option<Received>;

    /// This endpoint's own identifier.
    fn local_peer(&self) -> PeerId;

    /// Largest payload this transport accepts.
    fn mtu(&self) -> usize {
        MTU
    }
}

/// Drains every datagram available at `now`.
///
/// A convenience for the common loop; transports are polled until empty each tick.
pub fn drain<T: Transport + ?Sized>(t: &mut T, now: Timestamp) -> Vec<Received> {
    let mut out = Vec::new();
    while let Some(r) = t.recv(now) {
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_arithmetic_saturates() {
        assert_eq!(Timestamp::from_millis(5).as_micros(), 5_000);
        assert_eq!(Timestamp::from_micros(5_500).as_millis(), 5);
        assert_eq!(Timestamp(10).since(Timestamp(4)), 6);
        assert_eq!(
            Timestamp(4).since(Timestamp(10)),
            0,
            "elapsed time never goes negative"
        );
        assert_eq!(Timestamp(u64::MAX).plus_micros(10), Timestamp(u64::MAX));
    }
}
