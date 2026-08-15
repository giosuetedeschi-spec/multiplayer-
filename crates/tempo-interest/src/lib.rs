//! Interest management: which entities matter to which observer, and which to send now.
//!
//! Implements [ADR-0011](../../../docs/adr/0011-interest-management.md). Two cooperating
//! mechanisms, and both are needed:
//!
//! - [`aoi`] decides **what is relevant at all**. Sending every entity to every client is
//!   O(entities × clients) — five million entity-updates per snapshot at 10,000 entities and 500
//!   clients, which is impossible at any tick rate and pointless besides.
//! - [`priority`] decides **what to send now**, because filtering still leaves more relevant
//!   entities than the bandwidth budget can carry in a crowded area.
//!
//! # The property that matters
//!
//! Priority *accumulates*, so an entity skipped this tick is more likely to be chosen next tick.
//! Nothing starves. Bandwidth degrades into "important things update at full rate, unimportant
//! things update slowly" rather than "unimportant things never update" — which is the difference
//! between a game that feels congested and one that feels broken.
//!
//! This is also why the design is neither round-robin (which ignores that a duelling opponent
//! matters more than a distant crate) nor a distance cutoff (which starves everything past it
//! permanently).
//!
//! # Determinism
//!
//! Every ordering here is explicit. Grid queries return entities sorted by index, relevance changes
//! are sorted, and priority ties break on entity index. Hash-map iteration order would otherwise
//! leak into the replication order and make two identical states replicate differently — which
//! would break the reproducibility that capture-and-replay depends on.

#![forbid(unsafe_code)]

pub mod aoi;
pub mod priority;

pub use aoi::{InterestStrategy, RelevanceChange, RelevanceTracker, SpatialGrid};
pub use priority::{PriorityAccumulator, PriorityRule, Selection};
