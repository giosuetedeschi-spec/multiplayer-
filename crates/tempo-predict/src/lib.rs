//! Client prediction, server reconciliation, and clock synchronisation.
//!
//! These are the pieces that make an authoritative server feel local. Without them a client waits a
//! round trip before its own character moves, which players perceive as sluggish above roughly
//! 50 ms and broken above 100 ms.
//!
//! - [`Predictor`] — apply local input immediately, then correct when the authority disagrees.
//! - [`ClockSync`] — keep the client running just far enough ahead that its inputs arrive in time.
//!
//! Both are built on the core's snapshots, which is why the core owns state at all
//! ([ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md)).
//!
//! # Prediction needs determinism
//!
//! [`Predictor::reconcile`] takes the same simulation function the prediction used. If that function
//! is not deterministic — if it reads a clock, iterates a hash map, or uses floating point — every
//! tick will appear to diverge and the client will re-simulate constantly while feeling worse than
//! if it had never predicted at all. Use [`tempo_fixed`] types for anything simulated.

#![forbid(unsafe_code)]

pub mod clock;
pub mod predict;

pub use clock::{ClockSync, DilationHint};
pub use predict::{PredictedTick, Predictor, Reconciliation};
