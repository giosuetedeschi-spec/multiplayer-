//! The `tempo` core: the world arena, snapshots, and delta replication.
//!
//! This crate owns replicated game state. That inversion — the framework holding your state rather
//! than carrying your bytes — is the decision every other feature depends on, and the reasoning is
//! in [ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md).
//!
//! # What lives here
//!
//! - [`World`] — a columnar arena of replicated components, addressed by entity slot.
//! - [`WorldSnapshot`] and [`SnapshotRing`] — flat copies of that arena, which is what makes
//!   rollback, lag compensation and persistence all affordable from one mechanism.
//! - [`encode_delta`] and [`apply_delta`] — bit-packed deltas against a per-receiver baseline.
//!
//! # Example
//!
//! ```
//! use tempo_core::{apply_delta, encode_delta, World};
//! use tempo_fixed::{Fx, Vec2};
//! use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};
//!
//! let build = || {
//!     let mut w = World::new();
//!     let c = w.register(ComponentDesc::new("Player", vec![
//!         FieldDesc::new("position", FieldType::Vec2)
//!             .with_quantize(Fx::from_raw(0x418937), Fx::from_int(-1000), Fx::from_int(1000)),
//!     ])).unwrap();
//!     (w, c)
//! };
//!
//! let (mut server, player) = build();
//! let (mut client, _) = build();
//!
//! let e = server.spawn();
//! server.set_named(e, player, "position", &Value::Vec2(Vec2::from_ints(3, 4)))?;
//!
//! let delta = encode_delta(&server, None)?;
//! apply_delta(&mut client, &delta.bytes)?;
//!
//! // The client now holds the *quantized* position. It is within one step of the server's raw
//! // value, and deliberately not equal to it — see the note on quantization below.
//! let Value::Vec2(p) = client.get_named(e, player, "position")? else { unreachable!() };
//! assert!(p.sub(Vec2::from_ints(3, 4)).length() < Fx::from_raw(0x418937));
//!
//! // Nothing changed, so the next delta describes no entities at all.
//! assert_eq!(encode_delta(&server, Some(&delta.as_sent))?.entity_count, 0);
//! # Ok::<(), tempo_core::CoreError>(())
//! ```
//!
//! # Quantization means peers hold different bytes
//!
//! A sender holds raw values; a receiver holds quantized ones. Their arenas therefore differ by up
//! to half a quantization step per field, and **their [`World::state_hash`] values differ too**.
//! That is correct, not a desync.
//!
//! State hashes are comparable between peers running the *same simulation* — mesh peers under
//! rollback, or a client comparing its own prediction against its own history. They are **not**
//! comparable between a server's authoritative world and a client's replicated view of it. Code
//! that compares the two is asking the wrong question; compare against
//! [`DeltaResult::as_sent`] instead, which is the state the sender says the receiver will hold.

#![forbid(unsafe_code)]

pub mod delta;
pub mod entity;
pub mod layout;
pub mod snapshot;
pub mod world;

pub use delta::{apply_delta, encode_delta, DeltaResult};
pub use entity::{Entity, EntityAllocator, Tick};
pub use layout::{slot_size, ComponentLayout, FieldLayout};
pub use snapshot::{SnapshotRing, StateComparison, WorldSnapshot};
pub use world::{ComponentId, World};

use core::fmt;
use tempo_wire::{FieldType, WireError};

/// Errors produced by the core.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreError {
    /// An entity handle referred to a dead or recycled slot.
    StaleEntity(Entity),
    /// No component is registered with this identifier.
    UnknownComponent(u32),
    /// No field exists at this canonical index.
    UnknownField(usize),
    /// No field exists with this name.
    UnknownFieldName(String),
    /// The entity does not carry the requested component.
    ComponentNotPresent {
        /// The entity queried.
        entity: Entity,
        /// The component's name.
        component: String,
    },
    /// A field type cannot be stored in the arena.
    ///
    /// Variable-length fields would break the flat copy that snapshots depend on. They remain
    /// available for commands and RPCs.
    UnsupportedFieldType {
        /// The component being laid out.
        component: String,
        /// The offending field.
        field: String,
        /// Its declared type.
        ty: FieldType,
    },
    /// A value's type did not match the field's declaration.
    TypeMismatch {
        /// The component.
        component: String,
        /// The field.
        field: String,
        /// The declared type.
        expected: FieldType,
        /// The type supplied.
        found: FieldType,
    },
    /// Components were registered after the schema was frozen.
    SchemaFrozen,
    /// A snapshot's component count did not match the world's.
    SnapshotShapeMismatch {
        /// Components the world has.
        expected: usize,
        /// Components the snapshot has.
        found: usize,
    },
    /// A received delta was structurally invalid.
    MalformedDelta(String),
    /// An error from the wire codec.
    Wire(WireError),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::StaleEntity(e) => write!(f, "{e:?} is not alive"),
            CoreError::UnknownComponent(id) => write!(f, "no component with id {id}"),
            CoreError::UnknownField(i) => write!(f, "no field at canonical index {i}"),
            CoreError::UnknownFieldName(n) => write!(f, "no field named {n:?}"),
            CoreError::ComponentNotPresent { entity, component } => {
                write!(f, "{entity:?} does not have component {component}")
            }
            CoreError::UnsupportedFieldType {
                component,
                field,
                ty,
            } => write!(
                f,
                "{component}.{field}: {} cannot be stored in the arena; variable-length fields \
                 would break flat snapshots (see ADR-0027)",
                ty.canonical_name()
            ),
            CoreError::TypeMismatch {
                component,
                field,
                expected,
                found,
            } => write!(
                f,
                "{component}.{field}: schema declares {}, value is {}",
                expected.canonical_name(),
                found.canonical_name()
            ),
            CoreError::SchemaFrozen => {
                write!(
                    f,
                    "the schema is frozen; components cannot be registered after connecting"
                )
            }
            CoreError::SnapshotShapeMismatch { expected, found } => write!(
                f,
                "snapshot has {found} components but the world has {expected}"
            ),
            CoreError::MalformedDelta(d) => write!(f, "malformed delta: {d}"),
            CoreError::Wire(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for CoreError {}

impl From<WireError> for CoreError {
    fn from(e: WireError) -> CoreError {
        CoreError::Wire(e)
    }
}
