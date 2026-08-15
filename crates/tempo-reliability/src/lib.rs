//! Reliability: acknowledgements, delivery guarantees, and fragmentation.
//!
//! Four named channels over one unreliable datagram layer, with shared acknowledgement state.
//!
//! # Why this is not an external library
//!
//! Mature reliability layers exist — `laminar`, `ENet`, `renet`. They were not adopted because the
//! reliability layer must be co-designed with replication: delta compression has to know exactly
//! which snapshot each client has acknowledged in order to choose a baseline
//! ([ADR-0009](../../../docs/adr/0009-custom-bitpacked-wire-format.md)). Bolting that onto a
//! transport that owns its own acks means either maintaining two views of the same state or
//! reaching through an abstraction that was never built to expose it.
//!
//! Here [`AckTracker`] is the single source of truth, and the replication layer reads it directly.
//!
//! # Layout
//!
//! - [`sequence`] — wraparound-safe sequence numbers and the ring keyed by them
//! - [`ack`] — the ack bitfield and RFC 6298 round-trip estimation
//! - [`channel`] — the four delivery guarantees
//! - [`fragment`] — splitting and reassembling oversized messages, under bounded memory

#![forbid(unsafe_code)]

pub mod ack;
pub mod channel;
pub mod fragment;
pub mod sequence;

pub use ack::{AckTracker, RttEstimator, ACK_BITS};
pub use channel::{ChannelKind, ChannelSet, IncomingMessage, OutgoingMessage};
pub use fragment::{fragment, FragmentError, FragmentHeader, Reassembler, ReassemblyLimits};
pub use sequence::{distance, extend, is_newer, SequenceBuffer};
