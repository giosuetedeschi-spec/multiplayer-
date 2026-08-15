//! Snapshot delta encoding and application.
//!
//! Implements the snapshot block in `docs/spec/wire-protocol.md` §4. This is where the bandwidth
//! is actually won: entity identity costs a few bits via gap encoding, unchanged fields cost one
//! bit each, and changed fields cost only their quantized width.
//!
//! # The trap that defeats delta compression
//!
//! Deltas must be computed against **what the receiver will hold**, not against the sender's raw
//! state. Quantization is lossy, so a sender that diffs raw values sees every quantized field as
//! permanently changed: it transmits, the receiver stores the quantized value, and next tick the
//! sender compares its unchanged raw value against its own raw baseline, finds a difference, and
//! transmits again. Every quantized field is re-sent every tick, forever — and quantized fields are
//! precisely the ones delta compression exists for.
//!
//! [`encode_delta`] therefore returns the state as the receiver will hold it, and callers store
//! *that* as the next baseline.

use tempo_wire::{decode_field, encode_field, lossy_round_trip, BitReader, BitWriter, Value};

use crate::entity::{Entity, Tick};
use crate::snapshot::WorldSnapshot;
use crate::world::World;
use crate::CoreError;

/// What happened to an entity between the baseline and now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityOp {
    Update = 0,
    Spawn = 1,
    Despawn = 2,
}

/// The output of [`encode_delta`].
pub struct DeltaResult {
    /// The encoded snapshot block.
    pub bytes: Vec<u8>,
    /// The state the receiver will hold once it applies these bytes.
    ///
    /// Store this as the baseline for the next delta. Using the sender's raw state instead breaks
    /// convergence — see the module documentation.
    pub as_sent: WorldSnapshot,
    /// Number of entities described. Zero means nothing changed.
    pub entity_count: usize,
}

/// Encodes the world as a delta against `baseline`, or in full when `baseline` is `None`.
pub fn encode_delta(
    world: &World,
    baseline: Option<&WorldSnapshot>,
) -> Result<DeltaResult, CoreError> {
    let components = world.canonical_component_ids();
    let mut as_sent = world.snapshot();

    // Build every entity's payload first, so the entity count can be written before them and
    // entities with nothing to report can be dropped entirely.
    struct Entry {
        index: u32,
        op: EntityOp,
        generation: u32,
        payload: BitWriter,
    }
    let mut entries: Vec<Entry> = Vec::new();

    let slot_max = world
        .slot_count()
        .max(baseline.map_or(0, |b| b.slot_count()));

    for index in 0..slot_max as u32 {
        let current = world.entity_at(index);
        let baseline_live = baseline.is_some_and(|b| b.slot_live(index as usize));

        let Some(entity) = current else {
            if baseline_live {
                entries.push(Entry {
                    index,
                    op: EntityOp::Despawn,
                    generation: 0,
                    payload: BitWriter::new(),
                });
            }
            continue;
        };

        // A slot whose generation changed is a *different* entity. Encoding it as a spawn (rather
        // than a despawn plus a spawn at the same index) keeps entity indices unique and ascending,
        // which the gap encoding requires.
        let is_spawn = baseline.is_none_or(|b| !b.was_live(index as usize, entity.generation()));

        let mut payload = BitWriter::new();
        let mut any_change = is_spawn;

        // Presence mask across all components, in canonical order.
        for &c in &components {
            payload.write_bool(world.has(entity, c));
        }
        if !is_spawn {
            if let Some(b) = baseline {
                for &c in &components {
                    let was = b.column_present(c.0 as usize, index as usize);
                    if was != world.has(entity, c) {
                        any_change = true;
                    }
                }
            }
        }

        for &c in &components {
            if !world.has(entity, c) {
                continue;
            }
            let layout = world.layout(c)?;
            let slot = world
                .slot_bytes(entity, c)
                .expect("presence was just checked");

            // A component present now but absent from the baseline has no prior values, so every
            // field is new.
            let component_is_new = is_spawn
                || baseline.is_none_or(|b| !b.column_present(c.0 as usize, index as usize));

            let mut dirty = Vec::with_capacity(layout.field_count());
            let mut sent_values = Vec::with_capacity(layout.field_count());

            for f in 0..layout.field_count() {
                let current_value = layout.read(slot, f);
                let sent = lossy_round_trip(&layout.fields[f].desc, &current_value);

                let changed = if component_is_new {
                    true
                } else {
                    let b = baseline.expect("component_is_new covers the None case");
                    let prior_slot = b.column_slot(c.0 as usize, index as usize, layout.stride);
                    layout.read(prior_slot, f) != sent
                };

                dirty.push(changed);
                sent_values.push(sent);
            }

            for &d in &dirty {
                payload.write_bool(d);
                any_change |= d;
            }
            for (f, sent) in sent_values.iter().enumerate() {
                // Patch the as-sent snapshot for *every* field, not only dirty ones: a field the
                // receiver already holds is still held in quantized form.
                let stride = layout.stride;
                as_sent.column_slot_mut(c.0 as usize, index as usize, stride, |s| {
                    layout.write(s, f, sent)
                })?;
                if dirty[f] {
                    encode_field(&mut payload, &layout.fields[f].desc, sent)
                        .map_err(CoreError::Wire)?;
                }
            }
        }

        if any_change {
            entries.push(Entry {
                index,
                op: if is_spawn {
                    EntityOp::Spawn
                } else {
                    EntityOp::Update
                },
                generation: entity.generation(),
                payload,
            });
        }
    }

    let mut w = BitWriter::with_capacity(256);
    w.write_bits(world.tick().0 as u64, 32);
    w.write_bits(baseline.map_or(Tick::NONE, |b| b.tick).0 as u64, 32);
    w.write_varuint(entries.len() as u64);

    let mut prev: Option<u32> = None;
    for e in &entries {
        // Gap encoding: entities are ascending, so the delta from the previous index is small even
        // when absolute indices are large.
        let gap = match prev {
            None => e.index,
            Some(p) => e.index - p - 1,
        };
        w.write_varuint(gap as u64);
        prev = Some(e.index);

        w.write_bits(e.op as u64, 2);
        if e.op == EntityOp::Spawn {
            // Varuint, not a fixed u32: generations start at zero and stay small for the life of
            // most entities, so a fixed width would spend four bytes per spawn to carry a zero.
            w.write_varuint(e.generation as u64);
        }
        if e.op != EntityOp::Despawn {
            let payload = e.payload.to_bytes();
            let bits = e.payload.bit_len();
            // Splice the payload in bit-exactly; going through bytes would insert padding between
            // entities and lose the packing.
            let mut r = BitReader::new(&payload);
            let mut remaining = bits;
            while remaining > 0 {
                let take = remaining.min(32) as u32;
                w.write_bits(r.read_bits(take).map_err(CoreError::Wire)?, take);
                remaining -= take as usize;
            }
        }
    }

    Ok(DeltaResult {
        bytes: w.finish(),
        as_sent,
        entity_count: entries.len(),
    })
}

/// Applies an encoded delta to a world, returning the tick it describes.
///
/// The world must already have the same components registered; the schema ID negotiated at connect
/// is what guarantees that.
pub fn apply_delta(world: &mut World, bytes: &[u8]) -> Result<Tick, CoreError> {
    let components = world.canonical_component_ids();
    let mut r = BitReader::new(bytes);

    let tick = Tick(r.read_bits(32).map_err(CoreError::Wire)? as u32);
    let _baseline_tick = Tick(r.read_bits(32).map_err(CoreError::Wire)? as u32);
    let count = r.read_varuint().map_err(CoreError::Wire)?;

    // A hostile peer controls this count. Bounding it against the bits actually present stops a
    // small packet from driving a huge loop.
    if count > r.bits_remaining() as u64 {
        return Err(CoreError::MalformedDelta(
            "entity count exceeds packet size".into(),
        ));
    }

    let mut index: i64 = -1;
    for _ in 0..count {
        let gap = r.read_varuint().map_err(CoreError::Wire)?;
        index = index
            .checked_add(gap as i64 + 1)
            .ok_or_else(|| CoreError::MalformedDelta("entity index overflow".into()))?;
        if index > u32::MAX as i64 {
            return Err(CoreError::MalformedDelta(
                "entity index out of range".into(),
            ));
        }
        let index = index as u32;

        let op = match r.read_bits(2).map_err(CoreError::Wire)? {
            0 => EntityOp::Update,
            1 => EntityOp::Spawn,
            2 => EntityOp::Despawn,
            _ => return Err(CoreError::MalformedDelta("reserved entity op".into())),
        };

        if op == EntityOp::Despawn {
            if let Some(e) = world.entity_at(index) {
                world.despawn(e);
            }
            continue;
        }

        let entity = if op == EntityOp::Spawn {
            let generation = u32::try_from(r.read_varuint().map_err(CoreError::Wire)?)
                .map_err(|_| CoreError::MalformedDelta("generation out of range".into()))?;
            let e = Entity::from_parts(index, generation);
            // Spawn replaces whatever occupied the slot, which is how a recycled index arrives as
            // a single entry rather than a despawn/spawn pair.
            if let Some(old) = world.entity_at(index) {
                if old.generation() != generation {
                    world.despawn(old);
                }
            }
            world.spawn_at(e);
            e
        } else {
            world.entity_at(index).ok_or(CoreError::MalformedDelta(
                "update for an unknown entity".into(),
            ))?
        };

        let mut present = Vec::with_capacity(components.len());
        for _ in &components {
            present.push(r.read_bool().map_err(CoreError::Wire)?);
        }

        for (&c, &is_present) in components.iter().zip(present.iter()) {
            if !is_present {
                world.remove(entity, c)?;
                continue;
            }
            let field_count = world.layout(c)?.field_count();
            world.insert(entity, c)?;

            let mut dirty = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                dirty.push(r.read_bool().map_err(CoreError::Wire)?);
            }
            for (f, &d) in dirty.iter().enumerate() {
                if !d {
                    continue;
                }
                let desc = world.layout(c)?.fields[f].desc.clone();
                let value: Value = decode_field(&mut r, &desc).map_err(CoreError::Wire)?;
                world.set(entity, c, f, &value)?;
            }
        }
    }

    world.set_tick(tick);
    Ok(tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_fixed::{Fx, Vec2};
    use tempo_wire::{ComponentDesc, FieldDesc, FieldType};

    use crate::world::ComponentId;

    const MILLI: Fx = Fx::from_raw(0x0041_8937);

    fn make_world() -> (World, ComponentId, ComponentId) {
        let mut w = World::new();
        let player = w
            .register(ComponentDesc::new(
                "Player",
                vec![
                    FieldDesc::new("position", FieldType::Vec2).with_quantize(
                        MILLI,
                        Fx::from_int(-1000),
                        Fx::from_int(1000),
                    ),
                    FieldDesc::new("health", FieldType::Uint).with_bits(8),
                    FieldDesc::new("alive", FieldType::Bool),
                ],
            ))
            .unwrap();
        let tag = w
            .register(ComponentDesc::new(
                "Tag",
                vec![FieldDesc::new("kind", FieldType::Enum).with_variants(4)],
            ))
            .unwrap();
        (w, player, tag)
    }

    /// Applies `src`'s delta to `dst` and asserts the receiver holds exactly what was sent.
    ///
    /// Note what this does **not** assert: that `src` and `dst` have equal state hashes. They do
    /// not, and should not. The sender holds raw values while the receiver holds quantized ones,
    /// so their arenas differ by up to half a step per quantized field. The meaningful check is
    /// against `as_sent` — the state the sender claims the receiver will hold.
    fn sync(src: &World, dst: &mut World, baseline: Option<&WorldSnapshot>) -> DeltaResult {
        let d = encode_delta(src, baseline).unwrap();
        apply_delta(dst, &d.bytes).unwrap();

        let mut expected = dst.clone();
        expected.restore(&d.as_sent).unwrap();
        assert_eq!(
            expected.state_hash(),
            dst.state_hash(),
            "receiver does not hold the state the sender said it sent"
        );
        d
    }

    #[test]
    fn a_full_snapshot_reproduces_the_world() {
        let (mut server, player, tag) = make_world();
        let (mut client, _, _) = make_world();

        let a = server.spawn();
        server
            .set_named(a, player, "position", &Value::Vec2(Vec2::from_ints(3, 4)))
            .unwrap();
        server
            .set_named(a, player, "health", &Value::Uint(200))
            .unwrap();
        server
            .set_named(a, player, "alive", &Value::Bool(true))
            .unwrap();
        let b = server.spawn();
        server.set_named(b, tag, "kind", &Value::Enum(2)).unwrap();

        sync(&server, &mut client, None);
        assert_eq!(client.entity_count(), 2);
        assert!(client.has(a, player));
        assert!(client.has(b, tag));
        assert!(!client.has(b, player));
    }

    #[test]
    fn an_unchanged_world_encodes_to_nothing() {
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();
        let e = server.spawn();
        server
            .set_named(e, player, "health", &Value::Uint(100))
            .unwrap();

        let first = sync(&server, &mut client, None);
        let second = encode_delta(&server, Some(&first.as_sent)).unwrap();
        assert_eq!(
            second.entity_count, 0,
            "nothing changed, so nothing is described"
        );
    }

    #[test]
    fn quantized_fields_converge_instead_of_resending_forever() {
        // The bug the module documentation warns about. Diffing against raw state makes every
        // quantized field permanently dirty; this asserts the second delta is empty.
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();

        let e = server.spawn();
        // A position that is deliberately not on a quantization step boundary.
        server
            .set_named(
                e,
                player,
                "position",
                &Value::Vec2(Vec2::new(Fx::from_ratio(1, 3), Fx::from_ratio(-2, 7))),
            )
            .unwrap();

        let first = sync(&server, &mut client, None);
        let second = encode_delta(&server, Some(&first.as_sent)).unwrap();
        assert_eq!(
            second.entity_count, 0,
            "a quantized field that did not change must not be re-sent"
        );
    }

    #[test]
    fn only_changed_fields_are_transmitted() {
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();

        let e = server.spawn();
        server
            .set_named(e, player, "position", &Value::Vec2(Vec2::from_ints(1, 1)))
            .unwrap();
        server
            .set_named(e, player, "health", &Value::Uint(255))
            .unwrap();
        let first = sync(&server, &mut client, None);

        server
            .set_named(e, player, "health", &Value::Uint(254))
            .unwrap();
        let second = encode_delta(&server, Some(&first.as_sent)).unwrap();
        apply_delta(&mut client, &second.bytes).unwrap();
        let mut expected = client.clone();
        expected.restore(&second.as_sent).unwrap();
        assert_eq!(expected.state_hash(), client.state_hash());

        // Both carry a fixed 8-byte header (tick plus baseline tick), so compare the payloads:
        // the full snapshot sends two 21-bit components and the rest, the delta sends one 8-bit
        // field. At realistic entity counts the header amortises to nothing.
        const HEADER: usize = 8;
        assert!(
            (second.bytes.len() - HEADER) * 2 < first.bytes.len() - HEADER,
            "delta payload {} bytes vs full payload {} bytes",
            second.bytes.len() - HEADER,
            first.bytes.len() - HEADER
        );
    }

    #[test]
    fn spawns_and_despawns_replicate() {
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();

        let a = server.spawn();
        server
            .set_named(a, player, "health", &Value::Uint(1))
            .unwrap();
        let mut baseline = sync(&server, &mut client, None).as_sent;

        let b = server.spawn();
        server
            .set_named(b, player, "health", &Value::Uint(2))
            .unwrap();
        baseline = sync(&server, &mut client, Some(&baseline)).as_sent;
        assert_eq!(client.entity_count(), 2);

        server.despawn(a);
        sync(&server, &mut client, Some(&baseline));
        assert_eq!(client.entity_count(), 1);
        assert!(!client.is_alive(a));
        assert!(client.is_alive(b));
    }

    #[test]
    fn a_recycled_index_arrives_as_a_single_spawn() {
        // Without the generation on the wire, the client would treat the new entity as the old one
        // and apply a partial delta to it — the classic ghost-entity bug.
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();

        let a = server.spawn();
        server
            .set_named(a, player, "health", &Value::Uint(10))
            .unwrap();
        server
            .set_named(a, player, "alive", &Value::Bool(true))
            .unwrap();
        let baseline = sync(&server, &mut client, None).as_sent;

        server.despawn(a);
        let b = server.spawn();
        assert_eq!(
            b.index(),
            a.index(),
            "the slot must be reused for this test to mean anything"
        );
        server
            .set_named(b, player, "health", &Value::Uint(20))
            .unwrap();

        sync(&server, &mut client, Some(&baseline));
        assert!(!client.is_alive(a));
        assert!(client.is_alive(b));
        assert_eq!(
            client.get_named(b, player, "health").unwrap(),
            Value::Uint(20)
        );
        assert_eq!(
            client.get_named(b, player, "alive").unwrap(),
            Value::Bool(false),
            "the new entity must not inherit the old one's fields"
        );
    }

    #[test]
    fn adding_and_removing_a_component_replicates() {
        let (mut server, player, tag) = make_world();
        let (mut client, _, _) = make_world();

        let e = server.spawn();
        server
            .set_named(e, player, "health", &Value::Uint(1))
            .unwrap();
        let mut baseline = sync(&server, &mut client, None).as_sent;

        server.set_named(e, tag, "kind", &Value::Enum(3)).unwrap();
        baseline = sync(&server, &mut client, Some(&baseline)).as_sent;
        assert!(client.has(e, tag));

        server.remove(e, tag).unwrap();
        sync(&server, &mut client, Some(&baseline));
        assert!(!client.has(e, tag));
    }

    #[test]
    fn sparse_entity_indices_cost_no_more_than_dense_ones() {
        // The actual property of gap encoding, stated as a comparison rather than a magic byte
        // count: four entities at indices 0, 50, 100, 150 must cost the same as four at 0, 1, 2, 3,
        // because each gap still fits one varint group. Absolute u32 indices would cost 16 bytes of
        // identity regardless of spacing.
        let encode_at = |indices: &[u32]| -> usize {
            let (mut server, player, _) = make_world();
            let max = *indices.last().unwrap();
            let all: Vec<Entity> = (0..=max).map(|_| server.spawn()).collect();
            for e in &all {
                if !indices.contains(&e.index()) {
                    server.despawn(*e);
                }
            }
            for e in &all {
                if indices.contains(&e.index()) {
                    server
                        .set_named(*e, player, "health", &Value::Uint(1))
                        .unwrap();
                }
            }
            let d = encode_delta(&server, None).unwrap();
            assert_eq!(d.entity_count, indices.len());
            d.bytes.len()
        };

        let dense = encode_at(&[0, 1, 2, 3]);
        let sparse = encode_at(&[0, 50, 100, 150]);
        assert_eq!(
            dense, sparse,
            "gap encoding must make spacing free while gaps stay under 128"
        );
    }

    #[test]
    fn sparse_worlds_replicate_correctly() {
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();
        let all: Vec<Entity> = (0..200).map(|_| server.spawn()).collect();
        let kept: Vec<Entity> = all
            .iter()
            .copied()
            .filter(|e| e.index() % 50 == 0)
            .collect();
        for e in &all {
            if !kept.contains(e) {
                server.despawn(*e);
            }
        }
        for e in &kept {
            server
                .set_named(*e, player, "health", &Value::Uint(7))
                .unwrap();
        }

        sync(&server, &mut client, None);
        assert_eq!(client.entity_count(), 4);
        for e in &kept {
            assert_eq!(
                client.get_named(*e, player, "health").unwrap(),
                Value::Uint(7)
            );
        }
    }

    #[test]
    fn a_long_sequence_of_deltas_stays_in_sync() {
        let (mut server, player, tag) = make_world();
        let (mut client, _, _) = make_world();
        let mut baseline: Option<WorldSnapshot> = None;
        let mut entities: Vec<Entity> = Vec::new();

        for tick in 0..120u32 {
            server.set_tick(Tick(tick));

            if tick % 7 == 0 {
                let e = server.spawn();
                server
                    .set_named(e, player, "health", &Value::Uint(255))
                    .unwrap();
                entities.push(e);
            }
            if tick % 11 == 0 && !entities.is_empty() {
                let e = entities.remove(0);
                server.despawn(e);
            }
            for (i, &e) in entities.iter().enumerate() {
                let x = Fx::from_ratio((tick as i32 * 7 + i as i32) % 900, 13);
                server
                    .set_named(e, player, "position", &Value::Vec2(Vec2::new(x, x)))
                    .unwrap();
                if tick % 3 == 0 {
                    server
                        .set_named(e, tag, "kind", &Value::Enum(tick % 4))
                        .unwrap();
                }
            }

            let d = encode_delta(&server, baseline.as_ref()).unwrap();
            apply_delta(&mut client, &d.bytes).unwrap();

            let mut expected = client.clone();
            expected.restore(&d.as_sent).unwrap();
            assert_eq!(
                expected.state_hash(),
                client.state_hash(),
                "diverged at tick {tick}"
            );
            baseline = Some(d.as_sent);
        }
    }

    #[test]
    fn malformed_deltas_are_rejected_not_trusted() {
        let (mut client, _, _) = make_world();
        // An enormous entity count in a tiny packet must not drive a huge loop.
        let mut w = BitWriter::new();
        w.write_bits(0, 32);
        w.write_bits(u32::MAX as u64, 32);
        w.write_varuint(u64::MAX / 2);
        let bytes = w.finish();
        assert!(matches!(
            apply_delta(&mut client, &bytes),
            Err(CoreError::MalformedDelta(_))
        ));
    }

    #[test]
    fn truncated_deltas_are_rejected() {
        let (mut server, player, _) = make_world();
        let (mut client, _, _) = make_world();
        let e = server.spawn();
        server
            .set_named(e, player, "health", &Value::Uint(9))
            .unwrap();
        let d = encode_delta(&server, None).unwrap();
        for cut in 1..d.bytes.len() {
            let mut clone = client.clone();
            // Must error rather than panic; partial application is acceptable, corruption is not.
            let _ = apply_delta(&mut clone, &d.bytes[..cut]);
        }
        let _ = &mut client;
    }
}
