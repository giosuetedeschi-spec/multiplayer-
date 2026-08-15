//! `tempo` — a plug-and-play multiplayer engine.
//!
//! This is the crate to import. Everything else in the workspace is an implementation layer that
//! this one re-exports.
//!
//! ```
//! use tempo::prelude::*;
//!
//! #[derive(Replicate)]
//! struct Player {
//!     #[replicate(quantize = "0.001", min = "-1000", max = "1000", priority = 2.0)]
//!     position: Vec2,
//!     #[replicate(bits = 10)]
//!     score: u32,
//!     alive: bool,
//! }
//!
//! let mut world = World::new();
//! let players = world.register_component::<Player>()?;
//!
//! let e = world.spawn();
//! players.write(&mut world, e, &Player {
//!     position: Vec2::from_ints(3, 4),
//!     score: 700,
//!     alive: true,
//! })?;
//!
//! let read_back = players.read(&world, e)?;
//! assert_eq!(read_back.score, 700);
//! assert!(read_back.alive);
//! # Ok::<(), tempo::CoreError>(())
//! ```
//!
//! No IDL, no build step, no code generator to install. The struct *is* the schema
//! ([ADR-0003](../../../docs/adr/0003-native-first-schema-derivation.md)), and the framework
//! reduces it to a canonical form whose hash is negotiated on connect — so a Rust struct, a Python
//! class and a Go struct describing the same component agree by construction rather than by
//! convention.
//!
//! # What you get
//!
//! | | |
//! |---|---|
//! | [`World`] | The columnar arena holding replicated state |
//! | [`encode_delta`] / [`apply_delta`] | Bit-packed deltas against a per-receiver baseline |
//! | [`Predictor`] | Client prediction with server reconciliation |
//! | [`ClockSync`] | Keeps a client running just far enough ahead |
//! | [`MemoryNetwork`] | In-memory transport, and the simulated link |
//! | [`ChannelSet`] | Four delivery guarantees over unreliable datagrams |
//! | [`ConnectToken`] | Offline-validated authentication |
//!
//! # Two things that will surprise you
//!
//! **Replicated arithmetic uses [`Fx`], not `f32`.** Floating point cannot produce bit-identical
//! results across six languages and two architectures, and rollback needs exactly that
//! ([ADR-0002](../../../docs/adr/0002-fixed-point-determinism.md)). Rendering, audio and UI can use
//! floats freely; simulated state cannot.
//!
//! **`String` and `Vec<u8>` cannot be replicated fields.** Variable length would break the flat
//! snapshot copy that rollback and lag compensation are built on
//! ([ADR-0027](../../../docs/adr/0027-fixed-size-replicated-fields.md)). Send them as commands, or
//! mark the field `#[replicate(skip)]` to keep it local.

#![forbid(unsafe_code)]

// The derive macro emits `::tempo::…` paths. Inside this crate that name does not otherwise
// exist, so alias it to ourselves — the conventional fix for a crate that uses its own derive.
extern crate self as tempo;

use core::marker::PhantomData;

pub use tempo_core::{
    apply_delta, encode_delta, ComponentId, ComponentLayout, CoreError, DeltaResult, Entity,
    SnapshotRing, StateComparison, Tick, World, WorldSnapshot,
};
pub use tempo_derive::Replicate;
pub use tempo_fixed::{Fx, Quat, Vec2, Vec3};
pub use tempo_netcode::{ConnectToken, Key, NetcodeError, PrivateToken, ReplayWindow};
pub use tempo_predict::{ClockSync, DilationHint, PredictedTick, Predictor, Reconciliation};
pub use tempo_reliability::{
    AckTracker, ChannelKind, ChannelSet, FragmentHeader, IncomingMessage, OutgoingMessage,
    Reassembler, ReassemblyLimits, RttEstimator,
};
pub use tempo_transport::{
    drain, LinkConditions, MemoryNetwork, MemoryTransport, PeerId, Received, Timestamp, Transport,
    TransportError, UdpTransport, MTU,
};
pub use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Schema, SchemaId, Value, WireError};

/// Everything needed to write a game, in one import.
pub mod prelude {
    pub use crate::{
        apply_delta, encode_delta, ChannelKind, ClockSync, ComponentDesc, ComponentId, CoreError,
        Entity, FieldDesc, FieldType, Fx, LinkConditions, MemoryNetwork, PeerId, Predictor, Quat,
        Reconciliation, RegisterExt, Registered, Replicate, Tick, Timestamp, Transport, Value,
        Vec2, Vec3, World,
    };
}

/// A struct that can live in the world arena as a replicated component.
///
/// Implemented by `#[derive(Replicate)]`. Implementing it by hand is possible but rarely wise: the
/// derive keeps the schema, the reads and the writes consistent with each other, and a hand-written
/// mismatch between them is a bug that only shows up as corrupted state on a peer.
pub trait Replicate: Sized {
    /// The component's name, which is what the canonical schema is keyed on.
    const COMPONENT_NAME: &'static str;

    /// The component's schema.
    fn describe() -> ComponentDesc;

    /// Writes this value into an entity's component slot.
    fn write_into(
        &self,
        world: &mut World,
        entity: Entity,
        component: ComponentId,
    ) -> Result<(), CoreError>;

    /// Reads a value out of an entity's component slot.
    fn read_from(world: &World, entity: Entity, component: ComponentId) -> Result<Self, CoreError>;
}

/// A registered component, carrying its type.
///
/// Returned by [`RegisterExt::register`]. Holding the type means [`Registered::read`] and
/// [`Registered::write`] cannot be called with the wrong struct, which a bare [`ComponentId`]
/// would allow.
#[derive(Debug)]
pub struct Registered<C> {
    id: ComponentId,
    marker: PhantomData<fn() -> C>,
}

// Derived Clone and Copy would require `C: Clone`, which is not needed: the handle holds no `C`.
impl<C> Clone for Registered<C> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C> Copy for Registered<C> {}

impl<C: Replicate> Registered<C> {
    /// The underlying identifier, for the untyped API.
    #[inline]
    pub fn id(self) -> ComponentId {
        self.id
    }

    /// Writes a value, adding the component to the entity if it is absent.
    #[inline]
    pub fn write(self, world: &mut World, entity: Entity, value: &C) -> Result<(), CoreError> {
        value.write_into(world, entity, self.id)
    }

    /// Reads a value.
    #[inline]
    pub fn read(self, world: &World, entity: Entity) -> Result<C, CoreError> {
        C::read_from(world, entity, self.id)
    }

    /// True if the entity carries this component.
    #[inline]
    pub fn present(self, world: &World, entity: Entity) -> bool {
        world.has(entity, self.id)
    }

    /// Reads a value if present, rather than erroring.
    #[inline]
    pub fn try_read(self, world: &World, entity: Entity) -> Option<C> {
        if self.present(world, entity) {
            self.read(world, entity).ok()
        } else {
            None
        }
    }
}

/// Registers derived components on a [`World`].
pub trait RegisterExt {
    /// Registers a component type and returns a typed handle.
    ///
    /// Named `register_component` rather than `register` on purpose: `World` already has an
    /// inherent `register(ComponentDesc)`, and an inherent method silently wins over a trait one.
    /// A same-named extension would resolve to the untyped call and produce a confusing error at
    /// the call site rather than here.
    fn register_component<C: Replicate>(&mut self) -> Result<Registered<C>, CoreError>;
}

impl RegisterExt for World {
    fn register_component<C: Replicate>(&mut self) -> Result<Registered<C>, CoreError> {
        let id = self.register(C::describe())?;
        Ok(Registered {
            id,
            marker: PhantomData,
        })
    }
}
