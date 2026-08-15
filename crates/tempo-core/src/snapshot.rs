//! World snapshots: save, restore, and the ring buffer rollback runs on.
//!
//! Taking a snapshot copies contiguous buffers rather than traversing user objects. That is the
//! whole reason the arena exists ([ADR-0001](../../../docs/adr/0001-core-owns-replicated-state.md)),
//! and it is what makes rollback ([ADR-0013](../../../docs/adr/0013-rollback-model.md)), lag
//! compensation ([ADR-0014](../../../docs/adr/0014-lag-compensation.md)) and durable persistence
//! ([ADR-0007](../../../docs/adr/0007-ephemeral-core-pluggable-durability.md)) all affordable from
//! one mechanism.

use crate::entity::Tick;
use crate::world::World;
use crate::CoreError;

/// A complete copy of a world's replicated state at one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldSnapshot {
    /// The tick this state belongs to.
    pub tick: Tick,
    pub(crate) generations: Vec<u32>,
    pub(crate) alive: Vec<bool>,
    pub(crate) free: Vec<u32>,
    /// Per component, in registration order: raw bytes and per-slot presence.
    pub(crate) columns: Vec<(Vec<u8>, Vec<bool>)>,
}

impl WorldSnapshot {
    /// Number of entity slots covered.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.generations.len()
    }

    /// Approximate heap footprint, for capacity planning against the rollback window.
    pub fn size_bytes(&self) -> usize {
        self.generations.len() * 4
            + self.alive.len()
            + self.free.len() * 4
            + self
                .columns
                .iter()
                .map(|(d, p)| d.len() + p.len())
                .sum::<usize>()
    }

    /// True if slot `index` held a live entity with `generation`.
    pub(crate) fn was_live(&self, index: usize, generation: u32) -> bool {
        index < self.alive.len() && self.alive[index] && self.generations[index] == generation
    }

    /// True if slot `index` held any live entity.
    pub(crate) fn slot_live(&self, index: usize) -> bool {
        index < self.alive.len() && self.alive[index]
    }
}

impl World {
    /// Copies the entire replicated state.
    pub fn snapshot(&self) -> WorldSnapshot {
        let (generations, alive, free) = self.entities.raw();
        WorldSnapshot {
            tick: self.tick(),
            generations: generations.to_vec(),
            alive: alive.to_vec(),
            free: free.to_vec(),
            columns: self
                .columns
                .iter()
                .map(|c| (c.data.clone(), c.present.clone()))
                .collect(),
        }
    }

    /// Replaces the world's state with a snapshot.
    ///
    /// The schema is not part of a snapshot, so restoring into a world with a different component
    /// set is rejected rather than silently misinterpreting bytes.
    pub fn restore(&mut self, snap: &WorldSnapshot) -> Result<(), CoreError> {
        if snap.columns.len() != self.columns.len() {
            return Err(CoreError::SnapshotShapeMismatch {
                expected: self.columns.len(),
                found: snap.columns.len(),
            });
        }
        self.set_tick(snap.tick);
        self.entities
            .restore(&snap.generations, &snap.alive, &snap.free);
        for (col, (data, present)) in self.columns.iter_mut().zip(snap.columns.iter()) {
            col.data.clear();
            col.data.extend_from_slice(data);
            col.present.clear();
            col.present.extend_from_slice(present);
        }
        Ok(())
    }
}

/// A fixed-capacity ring of recent snapshots, indexed by tick.
///
/// Preallocated and never grown during play: allocating inside the rollback path would make frame
/// times unpredictable at exactly the moment they matter, which is the one thing rollback cannot
/// tolerate.
#[derive(Debug, Clone)]
pub struct SnapshotRing {
    slots: Vec<Option<WorldSnapshot>>,
}

impl SnapshotRing {
    /// Creates a ring holding `capacity` ticks of history.
    pub fn new(capacity: usize) -> SnapshotRing {
        assert!(capacity > 0, "a snapshot ring needs at least one slot");
        SnapshotRing {
            slots: vec![None; capacity],
        }
    }

    /// Number of ticks of history the ring can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Stores a snapshot, overwriting whatever occupied its slot.
    pub fn store(&mut self, snap: WorldSnapshot) {
        let i = snap.tick.0 as usize % self.slots.len();
        self.slots[i] = Some(snap);
    }

    /// Retrieves the snapshot for a tick, if it is still in the ring.
    ///
    /// Checks the stored tick rather than trusting the slot: after `capacity` ticks the slot holds
    /// a different tick's data, and returning that would be a silent, catastrophic wrong answer.
    pub fn get(&self, tick: Tick) -> Option<&WorldSnapshot> {
        let i = tick.0 as usize % self.slots.len();
        match &self.slots[i] {
            Some(s) if s.tick == tick => Some(s),
            _ => None,
        }
    }

    /// The oldest tick still retrievable, if any.
    pub fn oldest(&self) -> Option<Tick> {
        self.slots
            .iter()
            .flatten()
            .map(|s| s.tick)
            .reduce(|a, b| if b.is_newer_than(a) { a } else { b })
    }

    /// The newest tick stored, if any.
    pub fn newest(&self) -> Option<Tick> {
        self.slots
            .iter()
            .flatten()
            .map(|s| s.tick)
            .reduce(|a, b| if b.is_newer_than(a) { b } else { a })
    }

    /// Total heap footprint of the stored snapshots.
    pub fn size_bytes(&self) -> usize {
        self.slots.iter().flatten().map(|s| s.size_bytes()).sum()
    }

    /// Empties the ring.
    pub fn clear(&mut self) {
        for s in &mut self.slots {
            *s = None;
        }
    }
}

/// Baseline accessors used by delta encoding.
///
/// These read a component column by *registration* index, which is how the encoder addresses
/// storage. Out-of-range reads return an empty slot rather than panicking: a baseline may cover
/// fewer entity slots than the current world when entities were spawned after it was taken.
impl WorldSnapshot {
    /// True if slot `index` carried component `component` when this snapshot was taken.
    pub(crate) fn column_present(&self, component: usize, index: usize) -> bool {
        self.columns
            .get(component)
            .and_then(|(_, p)| p.get(index).copied())
            .unwrap_or(false)
    }

    /// The raw bytes of one component slot, or an all-zero slot if it is out of range.
    pub(crate) fn column_slot(&self, component: usize, index: usize, stride: usize) -> &[u8] {
        const ZEROS: [u8; 256] = [0; 256];
        match self.columns.get(component) {
            Some((data, _)) if (index + 1) * stride <= data.len() => {
                &data[index * stride..(index + 1) * stride]
            }
            _ => &ZEROS[..stride.min(ZEROS.len())],
        }
    }

    /// Applies `f` to one component slot, growing the column if needed.
    pub(crate) fn column_slot_mut<R>(
        &mut self,
        component: usize,
        index: usize,
        stride: usize,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> R {
        let (data, present) = &mut self.columns[component];
        let needed = (index + 1) * stride;
        if data.len() < needed {
            data.resize(needed, 0);
        }
        if present.len() <= index {
            present.resize(index + 1, false);
        }
        present[index] = true;
        f(&mut data[index * stride..needed])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_fixed::Fx;
    use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

    use crate::world::ComponentId;

    fn world() -> (World, ComponentId) {
        let mut w = World::new();
        let c = w
            .register(ComponentDesc::new(
                "Player",
                vec![
                    FieldDesc::new("health", FieldType::Fx),
                    FieldDesc::new("score", FieldType::Uint),
                ],
            ))
            .unwrap();
        (w, c)
    }

    #[test]
    fn restore_reproduces_state_exactly() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(100)))
            .unwrap();
        w.advance_tick();
        let snap = w.snapshot();
        let hash = w.state_hash();

        w.set_named(e, c, "health", &Value::Fx(Fx::from_int(1)))
            .unwrap();
        let other = w.spawn();
        w.set_named(other, c, "score", &Value::Uint(5)).unwrap();
        assert_ne!(w.state_hash(), hash);

        w.restore(&snap).unwrap();
        assert_eq!(
            w.state_hash(),
            hash,
            "restore must be exact, not approximate"
        );
        assert_eq!(w.tick(), snap.tick);
        assert!(
            !w.is_alive(other),
            "entities spawned after the snapshot must be gone"
        );
    }

    #[test]
    fn restoring_reinstates_the_free_list() {
        // Without the free list, the restored world would hand out different slots than the
        // original run did, and two peers replaying the same inputs would diverge.
        let (mut w, _) = world();
        let a = w.spawn();
        let b = w.spawn();
        w.despawn(a);
        let snap = w.snapshot();

        let next_original = w.spawn();
        w.restore(&snap).unwrap();
        let next_restored = w.spawn();

        assert_eq!(next_original.index(), next_restored.index());
        assert_eq!(next_original.generation(), next_restored.generation());
        assert!(w.is_alive(b));
    }

    #[test]
    fn resimulating_from_a_snapshot_matches_simulating_straight_through() {
        // The property rollback depends on, stated directly.
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "score", &Value::Uint(0)).unwrap();

        let step = |w: &mut World| {
            let v = match w.get_named(e, c, "score").unwrap() {
                Value::Uint(v) => v,
                _ => unreachable!(),
            };
            w.set_named(e, c, "score", &Value::Uint(v * 3 + 1)).unwrap();
            w.advance_tick();
        };

        for _ in 0..10 {
            step(&mut w);
        }
        let snap = w.snapshot();
        for _ in 0..20 {
            step(&mut w);
        }
        let straight_through = w.state_hash();

        w.restore(&snap).unwrap();
        for _ in 0..20 {
            step(&mut w);
        }
        assert_eq!(w.state_hash(), straight_through);
    }

    #[test]
    fn restoring_a_mismatched_shape_is_rejected() {
        let (a, _) = world();
        let snap = a.snapshot();
        let mut b = World::new();
        assert!(matches!(
            b.restore(&snap),
            Err(CoreError::SnapshotShapeMismatch { .. })
        ));
    }

    #[test]
    fn the_ring_refuses_to_return_a_recycled_slot() {
        // The failure this guards against is silent and catastrophic: returning a different tick's
        // state as if it were the requested one.
        let (mut w, c) = world();
        let e = w.spawn();
        let mut ring = SnapshotRing::new(4);

        for i in 0..4u32 {
            w.set_named(e, c, "score", &Value::Uint(i as u64)).unwrap();
            w.set_tick(Tick(i));
            ring.store(w.snapshot());
        }
        assert!(ring.get(Tick(0)).is_some());

        w.set_tick(Tick(4));
        ring.store(w.snapshot());
        assert!(
            ring.get(Tick(0)).is_none(),
            "tick 0's slot now holds tick 4"
        );
        assert!(ring.get(Tick(4)).is_some());
        assert_eq!(ring.oldest(), Some(Tick(1)));
        assert_eq!(ring.newest(), Some(Tick(4)));
    }

    #[test]
    fn ring_capacity_bounds_memory() {
        let (mut w, c) = world();
        let e = w.spawn();
        w.set_named(e, c, "score", &Value::Uint(1)).unwrap();
        let mut ring = SnapshotRing::new(8);
        for i in 0..1000u32 {
            w.set_tick(Tick(i));
            ring.store(w.snapshot());
        }
        assert_eq!(ring.capacity(), 8);
        assert!(ring.size_bytes() <= 8 * w.snapshot().size_bytes());
    }
}

/// How a world compares against a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateComparison {
    /// Every replicated field is within tolerance.
    Agrees,
    /// A field differs by more than the tolerance allows.
    Diverges {
        /// The entity that disagrees.
        entity: crate::Entity,
        /// The component's name.
        component: String,
        /// The field's name.
        field: String,
    },
    /// The two describe different sets of live entities.
    EntitySetDiffers {
        /// The slot that disagrees.
        index: u32,
    },
}

impl World {
    /// Compares this world against a snapshot, allowing `tolerance` of numeric drift per field.
    ///
    /// # Why a tolerance is required rather than optional
    ///
    /// A client's prediction is computed in raw fixed point, while an authoritative snapshot has
    /// been through quantization. The two are *supposed* to differ, by up to half a step per field.
    /// An exact comparison would report a divergence on every quantized field on every tick, and a
    /// client that reconciled on that would re-simulate constantly and never feel responsive.
    ///
    /// The tolerance is therefore the error the application is willing to live with, and choosing it
    /// is a gameplay decision: too tight and the client corrects visibly for no reason, too loose
    /// and genuine divergence goes unnoticed. One quantization step is the sensible floor.
    pub fn compare_with(
        &self,
        snap: &WorldSnapshot,
        tolerance: tempo_fixed::Fx,
    ) -> StateComparison {
        let slots = self.slot_count().max(snap.slot_count());
        for index in 0..slots as u32 {
            let here = self.entity_at(index);
            let there = snap.slot_live(index as usize);
            match (here, there) {
                (None, false) => continue,
                (Some(_), false) | (None, true) => {
                    return StateComparison::EntitySetDiffers { index }
                }
                (Some(entity), true) => {
                    if !snap.was_live(index as usize, entity.generation()) {
                        return StateComparison::EntitySetDiffers { index };
                    }
                    for c in self.canonical_component_ids() {
                        let layout = match self.layout(c) {
                            Ok(l) => l,
                            Err(_) => continue,
                        };
                        let mine_present = self.has(entity, c);
                        let theirs_present = snap.column_present(c.0 as usize, index as usize);
                        if mine_present != theirs_present {
                            return StateComparison::Diverges {
                                entity,
                                component: layout.desc.name.clone(),
                                field: "<presence>".into(),
                            };
                        }
                        if !mine_present {
                            continue;
                        }
                        let mine_slot = self.slot_bytes(entity, c).expect("presence checked");
                        let theirs_slot =
                            snap.column_slot(c.0 as usize, index as usize, layout.stride);

                        for f in 0..layout.field_count() {
                            let a = layout.read(mine_slot, f);
                            let b = layout.read(theirs_slot, f);
                            if !values_agree(&a, &b, tolerance) {
                                return StateComparison::Diverges {
                                    entity,
                                    component: layout.desc.name.clone(),
                                    field: layout.fields[f].desc.name.clone(),
                                };
                            }
                        }
                    }
                }
            }
        }
        StateComparison::Agrees
    }
}

/// True if two field values are equal, allowing numeric types to differ by up to `tolerance`.
fn values_agree(a: &tempo_wire::Value, b: &tempo_wire::Value, tolerance: tempo_fixed::Fx) -> bool {
    use tempo_fixed::Fx;
    use tempo_wire::Value;

    let near = |x: Fx, y: Fx| x.sub(y).abs().raw() <= tolerance.raw();
    match (a, b) {
        (Value::Fx(x), Value::Fx(y)) => near(*x, *y),
        (Value::Vec2(x), Value::Vec2(y)) => near(x.x, y.x) && near(x.y, y.y),
        (Value::Vec3(x), Value::Vec3(y)) => near(x.x, y.x) && near(x.y, y.y) && near(x.z, y.z),
        (Value::Quat(x), Value::Quat(y)) => {
            near(x.x, y.x) && near(x.y, y.y) && near(x.z, y.z) && near(x.w, y.w)
        }
        // Discrete values have no meaningful tolerance: a health of 99 is not nearly 100.
        _ => a == b,
    }
}
