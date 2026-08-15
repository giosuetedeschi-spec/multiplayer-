//! GGPO-class rollback over core-owned state.
//!
//! Rollback predicts *everyone's* inputs and corrects afterwards, which is what makes a remote
//! opponent's actions appear without the delay that prediction of your own input alone cannot hide.
//! It is well understood in the fighting-game community and essentially unavailable outside
//! purpose-built engines, because it demands two things most frameworks cannot provide:
//! bit-identical determinism, and state save/restore fast enough to run many times per second.
//!
//! `tempo` has both, and not by coincidence. Fixed-point arithmetic supplies the determinism; the
//! columnar arena makes save and restore a copy of contiguous buffers. Rollback is the feature
//! those two decisions were made for — see
//! [ADR-0013](../../../docs/adr/0013-rollback-model.md).
//!
//! # The rules your simulation must follow
//!
//! The step function must be a pure function of state and input:
//!
//! - no wall clock
//! - no unseeded randomness
//! - no unordered iteration (a `HashMap` walk is not deterministic)
//! - no I/O
//! - no floating point in anything replicated
//!
//! Side effects — particles, audio, achievements — must be deferred to confirmed frames, or they
//! fire again every time a frame is re-simulated.
//!
//! [`RollbackSession::sync_test`] enforces all of this by re-simulating each frame and comparing
//! the result. Run it in development and the violations surface on your machine instead of as a
//! desync in someone's ranked match.

#![forbid(unsafe_code)]

pub mod input;
pub mod session;

pub use input::{InputQueue, InputSource, PlayerId};
pub use session::{
    FrameResult, RollbackConfig, RollbackError, RollbackLimit, RollbackSession, RollbackStats,
};
