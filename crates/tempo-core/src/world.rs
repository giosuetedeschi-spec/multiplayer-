//! The columnar world arena.
//!
//! This is the decision from [ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md) made
//! concrete. Replicated state lives here, in flat little-endian byte columns indexed by entity
//! slot, and three otherwise-hard features fall out of that layout:
//!
//! - **Rollback** save and restore is a copy of contiguous buffers, not a traversal of user objects.
//! - **Delta compression** can diff a column against a baseline column without knowing what the
//!   fields mean.
//! - **Lag compensation** can keep a ring of past worlds cheaply enough to rewind per hit query.
//!
//! # Storage shape
//!
//! Each component is one `Vec<u8>` of `stride` bytes per entity **slot**, addressed directly by
//! slot index, plus a presence flag per slot. Direct addressing rather than a sparse-to-dense map
//! costs memory when few entities carry a component — the trade buys O(1) lookup and, more
//! importantly, a snapshot that is a flat copy with no index rebuilding. Since snapshots are taken
//! tens of times per second under rollback and sparse component sets are the exception in practice,
//! the trade is worth making. It is revisitable per component if a profile ever says otherwise.

use blake3::Hasher;
use tempo_wire::{ComponentDesc, Schema, SchemaId, Value};

use crate::entity::{Entity, EntityAllocator, Tick};
use crate::layout::ComponentLayout;
use crate::CoreError;

/// Identifies a registered component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentId(pub u32);

/// One component's storage: a flat byte column plus per-slot presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    pub(crate) data: Vec<u8>,
    pub(crate) present: Vec<bool>,
    pub(crate) stride: usize,
}

impl Column {
    fn new(stride: usize) -> Column {
        Column {
            data: Vec::new(),
            present: Vec::new(),
            stride,
        }
    }

    fn ensure_slots(&mut self, slots: usize) {
        if self.present.len() < slots {
            self.present.resize(slots, false);
            // A stride of zero is legal: a component whose every field quantizes to zero bits.
            self.data.resize(slots * self.stride, 0);
        }
    }

    #[inline]
    fn slot(&self, index: usize) -> &[u8] {
        let start = index * self.stride;
        &self.data[start..start + self.stride]
    }

    #[inline]
    fn slot_mut(&mut self, index: usize) -> &mut [u8] {
        let start = index * self.stride;
        &mut self.data[start..start + self.stride]
    }
}

/// The replicated world.
#[derive(Debug, Clone)]
pub struct World {
    schema: Schema,
    layouts: Vec<ComponentLayout>,
    pub(crate) columns: Vec<Column>,
    pub(crate) entities: EntityAllocator,
    tick: Tick,
    frozen: bool,
}

impl Default for World {
    fn default() -> World {
        World::new()
    }
}

impl World {
    /// Creates an empty world with no registered components.
    pub fn new() -> World {
        World {
            schema: Schema::new(),
            layouts: Vec::new(),
            columns: Vec::new(),
            entities: EntityAllocator::new(),
            tick: Tick::ZERO,
            frozen: false,
        }
    }

    /// Registers a component and returns its identifier.
    ///
    /// Registration order determines [`ComponentId`] values, which are local. The *wire* order is
    /// canonical (name-sorted), so two peers registering in different orders still agree.
    pub fn register(&mut self, desc: ComponentDesc) -> Result<ComponentId, CoreError> {
        if self.frozen {
            return Err(CoreError::SchemaFrozen);
        }
        let layout = ComponentLayout::new(desc.clone())?;
        self.schema.register(desc).map_err(CoreError::Wire)?;
        let id = ComponentId(self.layouts.len() as u32);
        self.columns.push(Column::new(layout.stride));
        self.layouts.push(layout);
        let slots = self.entities.slot_count();
        self.columns
            .last_mut()
            .expect("just pushed")
            .ensure_slots(slots);
        Ok(id)
    }

    /// Prevents further registration.
    ///
    /// Called once a connection is attempted: changing the schema afterwards would change the
    /// negotiated ID underneath a live peer.
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// True once [`World::freeze`] has been called.
    #[inline]
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// The schema.
    #[inline]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The negotiated schema identifier.
    #[inline]
    pub fn schema_id(&self) -> SchemaId {
        self.schema.schema_id()
    }

    /// Looks up a component by name.
    pub fn component_id(&self, name: &str) -> Option<ComponentId> {
        self.layouts
            .iter()
            .position(|l| l.desc.name == name)
            .map(|i| ComponentId(i as u32))
    }

    /// The layout of a registered component.
    #[inline]
    pub fn layout(&self, c: ComponentId) -> Result<&ComponentLayout, CoreError> {
        self.layouts
            .get(c.0 as usize)
            .ok_or(CoreError::UnknownComponent(c.0))
    }

    /// All component layouts, in registration order.
    #[inline]
    pub fn layouts(&self) -> &[ComponentLayout] {
        &self.layouts
    }

    /// Component identifiers in **canonical (name-sorted)** order, which is wire order.
    pub fn canonical_component_ids(&self) -> Vec<ComponentId> {
        let mut ids: Vec<ComponentId> = (0..self.layouts.len() as u32).map(ComponentId).collect();
        ids.sort_by(|a, b| {
            self.layouts[a.0 as usize]
                .desc
                .name
                .as_bytes()
                .cmp(self.layouts[b.0 as usize].desc.name.as_bytes())
        });
        ids
    }

    /// The current tick.
    #[inline]
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// Sets the current tick.
    #[inline]
    pub fn set_tick(&mut self, tick: Tick) {
        self.tick = tick;
    }

    /// Advances to the next tick.
    #[inline]
    pub fn advance_tick(&mut self) {
        self.tick = self.tick.next();
    }

    /// Number of live entities.
    #[inline]
    pub fn entity_count(&self) -> usize {
        self.entities.live_count()
    }

    /// Number of allocated entity slots, live or not.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.entities.slot_count()
    }

    /// Iterates live entities in ascending slot order, which is wire order.
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.entities.iter()
    }

    /// True if the handle refers to a live entity.
    #[inline]
    pub fn is_alive(&self, e: Entity) -> bool {
        self.entities.is_alive(e)
    }

    /// The live entity occupying a slot index, if any.
    ///
    /// This is how the wire format addresses entities: it transmits slot indices, and the receiver
    /// resolves them here.
    #[inline]
    pub fn entity_at(&self, index: u32) -> Option<Entity> {
        self.entities.entity_at(index)
    }

    /// Spawns an entity with no components.
    pub fn spawn(&mut self) -> Entity {
        let e = self.entities.alloc();
        self.grow_to_fit();
        e
    }

    /// Spawns an entity with an identity chosen elsewhere, for applying a remote spawn.
    pub fn spawn_at(&mut self, e: Entity) {
        self.entities.alloc_at(e);
        self.grow_to_fit();
    }

    /// Despawns an entity and clears its components. Returns false if the handle was stale.
    pub fn despawn(&mut self, e: Entity) -> bool {
        if !self.entities.is_alive(e) {
            return false;
        }
        let i = e.index() as usize;
        for col in &mut self.columns {
            if i < col.present.len() {
                col.present[i] = false;
                // Zeroing rather than leaving stale bytes keeps the state hash a function of live
                // state alone. Otherwise two peers reaching the same state by different histories
                // would hash differently and report a desync that is not one.
                col.slot_mut(i).fill(0);
            }
        }
        self.entities.free(e)
    }

    /// Adds a component to an entity, zero-initialised. Idempotent.
    pub fn insert(&mut self, e: Entity, c: ComponentId) -> Result<(), CoreError> {
        self.check_alive(e)?;
        let stride = self.layout(c)?.stride;
        let col = &mut self.columns[c.0 as usize];
        let i = e.index() as usize;
        col.ensure_slots(i + 1);
        if !col.present[i] {
            col.present[i] = true;
            let start = i * stride;
            col.data[start..start + stride].fill(0);
        }
        Ok(())
    }

    /// Removes a component from an entity. Returns whether it was present.
    pub fn remove(&mut self, e: Entity, c: ComponentId) -> Result<bool, CoreError> {
        self.check_alive(e)?;
        self.layout(c)?;
        let col = &mut self.columns[c.0 as usize];
        let i = e.index() as usize;
        if i >= col.present.len() || !col.present[i] {
            return Ok(false);
        }
        col.present[i] = false;
        col.slot_mut(i).fill(0);
        Ok(true)
    }

    /// True if the entity carries the component.
    pub fn has(&self, e: Entity, c: ComponentId) -> bool {
        if !self.entities.is_alive(e) {
            return false;
        }
        self.columns
            .get(c.0 as usize)
            .and_then(|col| col.present.get(e.index() as usize).copied())
            .unwrap_or(false)
    }

    /// Reads a field by canonical index.
    pub fn get(&self, e: Entity, c: ComponentId, field: usize) -> Result<Value, CoreError> {
        self.check_alive(e)?;
        let layout = self.layout(c)?;
        if field >= layout.field_count() {
            return Err(CoreError::UnknownField(field));
        }
        let col = &self.columns[c.0 as usize];
        let i = e.index() as usize;
        if i >= col.present.len() || !col.present[i] {
            return Err(CoreError::ComponentNotPresent {
                entity: e,
                component: layout.desc.name.clone(),
            });
        }
        Ok(layout.read(col.slot(i), field))
    }

    /// Reads a field by name.
    pub fn get_named(&self, e: Entity, c: ComponentId, field: &str) -> Result<Value, CoreError> {
        let idx = self
            .layout(c)?
            .field_index(field)
            .ok_or_else(|| CoreError::UnknownFieldName(field.to_owned()))?;
        self.get(e, c, idx)
    }

    /// Writes a field by canonical index. Inserts the component if absent.
    pub fn set(
        &mut self,
        e: Entity,
        c: ComponentId,
        field: usize,
        value: &Value,
    ) -> Result<(), CoreError> {
        self.check_alive(e)?;
        if field >= self.layout(c)?.field_count() {
            return Err(CoreError::UnknownField(field));
        }
        self.insert(e, c)?;
        let layout = &self.layouts[c.0 as usize];
        let col = &mut self.columns[c.0 as usize];
        layout.write(col.slot_mut(e.index() as usize), field, value)
    }

    /// Writes a field by name.
    pub fn set_named(
        &mut self,
        e: Entity,
        c: ComponentId,
        field: &str,
        value: &Value,
    ) -> Result<(), CoreError> {
        let idx = self
            .layout(c)?
            .field_index(field)
            .ok_or_else(|| CoreError::UnknownFieldName(field.to_owned()))?;
        self.set(e, c, idx, value)
    }

    /// A read-only view of a component's raw bytes for one entity.
    ///
    /// This is the zero-copy read path the language bindings expose: the caller walks the bytes
    /// itself rather than calling back into the core per field.
    pub fn slot_bytes(&self, e: Entity, c: ComponentId) -> Option<&[u8]> {
        if !self.has(e, c) {
            return None;
        }
        Some(self.columns[c.0 as usize].slot(e.index() as usize))
    }

    /// The whole byte column for a component, `stride` bytes per slot.
    pub fn column_bytes(&self, c: ComponentId) -> Result<&[u8], CoreError> {
        self.layout(c)?;
        Ok(&self.columns[c.0 as usize].data)
    }

    /// Per-slot presence flags for a component.
    pub fn column_present(&self, c: ComponentId) -> Result<&[bool], CoreError> {
        self.layout(c)?;
        Ok(&self.columns[c.0 as usize].present)
    }

    /// A BLAKE3 hash of all replicated state, for desync detection.
    ///
    /// Deliberately excludes the tick, so two peers can compare the state they believe holds *at*
    /// a tick. Walks components in canonical order and entities in ascending slot order, so the
    /// hash depends only on state — never on registration or spawn order.
    ///
    /// # What it can and cannot be compared against
    ///
    /// Two hashes are comparable only when both worlds hold values in the same representation.
    /// That covers peers running the same simulation — mesh peers under rollback, or a client
    /// checking its own prediction against its own history.
    ///
    /// It does **not** cover a server's authoritative world against a client's replicated view.
    /// The server holds raw values while the client holds quantized ones, so the two hashes differ
    /// by design. Compare the client against `DeltaResult::as_sent` instead.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = Hasher::new();
        for c in self.canonical_component_ids() {
            let layout = &self.layouts[c.0 as usize];
            h.update(layout.desc.name.as_bytes());
            let col = &self.columns[c.0 as usize];
            for e in self.entities.iter() {
                let i = e.index() as usize;
                if i < col.present.len() && col.present[i] {
                    h.update(&e.index().to_le_bytes());
                    h.update(&e.generation().to_le_bytes());
                    h.update(col.slot(i));
                }
            }
        }
        *h.finalize().as_bytes()
    }

    /// Removes all entities and component data, keeping the schema.
    pub fn clear_entities(&mut self) {
        self.entities.clear();
        for col in &mut self.columns {
            col.data.clear();
            col.present.clear();
        }
    }

    fn grow_to_fit(&mut self) {
        let slots = self.entities.slot_count();
        for col in &mut self.columns {
            col.ensure_slots(slots);
        }
    }

    fn check_alive(&self, e: Entity) -> Result<(), CoreError> {
        if self.entities.is_alive(e) {
            Ok(())
        } else {
            Err(CoreError::StaleEntity(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_fixed::{Fx, Vec2};
    use tempo_wire::{FieldDesc, FieldType};

    fn world() -> (World, ComponentId) {
        let mut w = World::new();
        let c = w
            .register(ComponentDesc::new(
                "Player",
                vec![
                    FieldDesc::new("position", FieldType::Vec2),
                    FieldDesc::new("health", FieldType::Fx),
                    FieldDesc::new("alive", FieldType::Bool),
                ],
            ))
            .unwrap();
        (w, c)
    }

    #[test]
    fn spawn_set_get_round_trip() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "position", &Value::Vec2(Vec2::from_ints(3, 4)))
            .unwrap();
        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(100)))
            .unwrap();
        assert_eq!(
            w.get_named(e, c, "position").unwrap(),
            Value::Vec2(Vec2::from_ints(3, 4))
        );
        assert_eq!(
            w.get_named(e, c, "health").unwrap(),
            Value::Fx(Fx::from_int(100))
        );
        // A field never written reads as its zero value, not an error.
        assert_eq!(w.get_named(e, c, "alive").unwrap(), Value::Bool(false));
    }

    #[test]
    fn stale_handles_are_rejected_rather_than_aliasing() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(50)))
            .unwrap();
        w.despawn(e);
        let reused = w.spawn();
        assert_eq!(reused.index(), e.index(), "the slot is reused");

        assert!(matches!(
            w.get_named(e, c, "health"),
            Err(CoreError::StaleEntity(_))
        ));
        assert!(matches!(
            w.set_named(e, c, "health", &Value::Fx(Fx::ONE)),
            Err(CoreError::StaleEntity(_))
        ));
        // And crucially, the reused slot did not inherit the dead entity's data.
        assert!(!w.has(reused, c));
    }

    #[test]
    fn despawn_zeroes_storage_so_the_hash_depends_only_on_live_state() {
        // Two worlds reaching the same live state by different histories must hash identically,
        // or peers report a desync that is not one.
        let (mut a, ca) = world();
        let tmp = a.spawn();
        a.set_named(tmp, ca, "health", &Value::Fx(Fx::from_int(77)))
            .unwrap();
        a.despawn(tmp);
        let e = a.spawn();
        a.set_named(e, ca, "health", &Value::Fx(Fx::from_int(10)))
            .unwrap();

        let (mut b, cb) = world();
        let tmp2 = b.spawn();
        b.despawn(tmp2);
        let e2 = b.spawn();
        b.set_named(e2, cb, "health", &Value::Fx(Fx::from_int(10)))
            .unwrap();

        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn the_hash_ignores_registration_and_spawn_order() {
        let mut a = World::new();
        let ca = a
            .register(ComponentDesc::new(
                "Alpha",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();
        let cb = a
            .register(ComponentDesc::new(
                "Beta",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();

        let mut b = World::new();
        let cb2 = b
            .register(ComponentDesc::new(
                "Beta",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();
        let ca2 = b
            .register(ComponentDesc::new(
                "Alpha",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();

        for (w, x, y) in [(&mut a, ca, cb), (&mut b, ca2, cb2)] {
            let e = w.spawn();
            w.set(e, x, 0, &Value::Uint(1)).unwrap();
            w.set(e, y, 0, &Value::Uint(2)).unwrap();
        }
        assert_eq!(a.state_hash(), b.state_hash());
        assert_eq!(a.schema_id(), b.schema_id());
    }

    #[test]
    fn the_hash_changes_when_state_changes() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(100)))
            .unwrap();
        let before = w.state_hash();
        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(99)))
            .unwrap();
        assert_ne!(before, w.state_hash());
    }

    #[test]
    fn the_hash_ignores_the_tick() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "health", &Value::Fx(Fx::ONE)).unwrap();
        let before = w.state_hash();
        w.advance_tick();
        assert_eq!(
            before,
            w.state_hash(),
            "peers compare state at a tick, not the tick"
        );
    }

    #[test]
    fn components_are_independent() {
        let mut w = World::new();
        let a = w
            .register(ComponentDesc::new(
                "A",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();
        let b = w
            .register(ComponentDesc::new(
                "B",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();
        let e = w.spawn();
        w.set(e, a, 0, &Value::Uint(1)).unwrap();
        assert!(w.has(e, a));
        assert!(!w.has(e, b));
        assert!(matches!(
            w.get(e, b, 0),
            Err(CoreError::ComponentNotPresent { .. })
        ));

        assert!(w.remove(e, a).unwrap());
        assert!(!w.remove(e, a).unwrap(), "removing twice reports absent");
    }

    #[test]
    fn registering_after_freeze_is_rejected() {
        let (mut w, _) = world();
        w.freeze();
        let err = w.register(ComponentDesc::new(
            "Late",
            vec![FieldDesc::new("v", FieldType::Bool)],
        ));
        assert!(matches!(err, Err(CoreError::SchemaFrozen)));
    }

    #[test]
    fn components_registered_after_entities_exist_still_get_storage() {
        let (mut w, _) = world();
        let e = w.spawn();
        let late = w
            .register(ComponentDesc::new(
                "Late",
                vec![FieldDesc::new("v", FieldType::Uint)],
            ))
            .unwrap();
        w.set(e, late, 0, &Value::Uint(5)).unwrap();
        assert_eq!(w.get(e, late, 0).unwrap(), Value::Uint(5));
    }

    #[test]
    fn spawn_at_reproduces_a_remote_identity() {
        let (mut w, c) = world();
        let remote = Entity::from_parts(42, 7);
        w.spawn_at(remote);
        w.set_named(remote, c, "health", &Value::Fx(Fx::ONE))
            .unwrap();
        assert!(w.is_alive(remote));
        assert_eq!(w.slot_count(), 43);
    }

    #[test]
    fn unknown_names_and_ids_report_clearly() {
        let (mut w, c) = world();
        let e = w.spawn();
        assert!(matches!(
            w.get_named(e, c, "nope"),
            Err(CoreError::UnknownFieldName(_))
        ));
        assert!(matches!(w.get(e, c, 99), Err(CoreError::UnknownField(99))));
        assert!(matches!(
            w.layout(ComponentId(99)),
            Err(CoreError::UnknownComponent(99))
        ));
    }
}
