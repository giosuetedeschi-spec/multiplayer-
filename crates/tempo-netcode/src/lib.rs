//! Connection security: connect tokens, packet encryption, and replay protection.
//!
//! Implements [ADR-0015](../../../docs/adr/0015-connect-tokens-and-security.md), following the
//! netcode.io design rather than inventing one — novel cryptographic protocol design is how security
//! bugs happen, and netcode.io matches these requirements closely.
//!
//! # The problem this solves
//!
//! A UDP game server exposed to the internet faces problems a TCP service does not. Anyone can send
//! a packet claiming to be anyone; source addresses are trivially forged; a small request producing
//! a large response makes the server a DDoS reflector; captured packets can be resent.
//!
//! Meanwhile the game server should *not* be doing authentication. It should not hold credentials,
//! should not query an auth database on the connect path, and should not become unavailable when the
//! auth service does.
//!
//! # The shape of the answer
//!
//! The client authenticates with the game's own backend. The backend, holding a key shared with the
//! game servers, issues a [`ConnectToken`]. The client presents it to a server, which validates it
//! **offline** — no backend round trip, no credentials, no shared availability.
//!
//! # What this is not
//!
//! Connect tokens authenticate *who is connecting*. They say nothing about whether a connected
//! client behaves honestly. Anti-cheat is the authoritative simulation's job — the server accepts
//! inputs, never state — and in mesh topologies there is no such protection at all
//! ([ADR-0006](../../../docs/adr/0006-topology-agnostic-api.md)).

#![forbid(unsafe_code)]

pub mod crypto;
pub mod token;

pub use crypto::{open, seal, Key, ReplayWindow, TAG_BYTES};
pub use token::{ConnectToken, PrivateToken, DEFAULT_EXPIRY_SECONDS, USER_DATA_BYTES};

use core::fmt;

/// Errors from the connection layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetcodeError {
    /// Authentication or decryption failed.
    ///
    /// Deliberately carries no detail. Reporting *why* verification failed would give an attacker
    /// an oracle to probe with.
    Crypto,
    /// A token was structurally invalid.
    MalformedToken,
    /// A token's validity period has passed.
    TokenExpired {
        /// Unix second the token expired.
        expired_at: u64,
        /// Unix second the check was made.
        now: u64,
    },
    /// A token was issued for a different game or protocol version.
    ProtocolMismatch {
        /// What this server expects.
        expected: u64,
        /// What the token carries.
        found: u64,
    },
    /// The operating system's entropy source failed.
    Entropy(String),
}

impl fmt::Display for NetcodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetcodeError::Crypto => write!(f, "authentication failed"),
            NetcodeError::MalformedToken => write!(f, "malformed connect token"),
            NetcodeError::TokenExpired { expired_at, now } => {
                write!(f, "connect token expired at {expired_at}, now {now}")
            }
            NetcodeError::ProtocolMismatch { expected, found } => {
                write!(
                    f,
                    "token is for protocol {found}, this server serves {expected}"
                )
            }
            NetcodeError::Entropy(e) => write!(f, "entropy source failed: {e}"),
        }
    }
}

impl core::error::Error for NetcodeError {}
