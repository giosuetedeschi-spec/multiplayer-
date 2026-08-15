//! Tests for `#[derive(Replicate)]`, exercised the way a user sees it.
//!
//! Deliberately an integration test rather than a unit test: from here `tempo` is an external
//! crate, so the paths the derive macro emits are resolved exactly as they would be in a real
//! project. An in-crate test would resolve them differently and could pass while the public path
//! was broken.

use tempo::prelude::*;

#[derive(Replicate, Debug, PartialEq)]
struct Body {
    #[replicate(quantize = "0.001", min = "-1000", max = "1000", priority = 2.0)]
    position: Vec2,
    #[replicate(quantize = "0.01", min = "-50", max = "50")]
    velocity: Vec2,
    #[replicate(bits = 10)]
    health: u32,
    alive: bool,
}

#[derive(Replicate, Debug, PartialEq)]
struct Tag {
    #[replicate(bits = 4)]
    kind: u8,
    #[replicate(skip)]
    local_only: u64,
}

#[test]
fn a_derived_struct_round_trips_through_the_arena() {
    let mut world = World::new();
    let bodies = world.register_component::<Body>().unwrap();

    let e = world.spawn();
    let value = Body {
        position: Vec2::from_ints(3, -4),
        velocity: Vec2::from_ints(1, 2),
        health: 900,
        alive: true,
    };
    bodies.write(&mut world, e, &value).unwrap();

    let back = bodies.read(&world, e).unwrap();
    assert_eq!(back.health, 900);
    assert!(back.alive);
    // Position is quantized on the way in, so it comes back near, not equal.
    assert!(back.position.sub(value.position).length() < Fx::from_raw(0x0041_8937));
}

#[test]
fn the_derived_schema_matches_a_hand_written_one() {
    // The derive is a convenience over the same descriptors, not a parallel implementation.
    let derived = Body::describe();
    let manual = ComponentDesc::new(
        "Body",
        vec![
            FieldDesc::new("position", FieldType::Vec2).with_quantize(
                Fx::from_raw(0x0041_8937),
                Fx::from_int(-1000),
                Fx::from_int(1000),
            ),
            FieldDesc::new("velocity", FieldType::Vec2).with_quantize(
                Fx::from_raw(0x0000_028F_5C29),
                Fx::from_int(-50),
                Fx::from_int(50),
            ),
            FieldDesc::new("health", FieldType::Uint).with_bits(10),
            FieldDesc::new("alive", FieldType::Bool),
        ],
    );
    let mut a = tempo_wire::Schema::new();
    a.register(derived).unwrap();
    let mut b = tempo_wire::Schema::new();
    b.register(manual).unwrap();
    assert_eq!(a.canonical_form(), b.canonical_form());
}

#[test]
fn quantization_strings_become_integer_literals() {
    // The promise of ADR-0002: floats are resolved during macro expansion, so no float reaches
    // the generated code or the runtime. 0.001 * 2^32 rounds to 0x418937.
    let desc = Body::describe();
    let position = desc.fields.iter().find(|f| f.name == "position").unwrap();
    assert_eq!(position.quantize.unwrap().raw(), 0x0041_8937);
    assert_eq!(position.min.unwrap(), Fx::from_int(-1000));
    assert_eq!(position.base_priority, 2.0);
}

#[test]
fn skipped_fields_are_absent_from_the_schema() {
    let desc = Tag::describe();
    assert_eq!(desc.fields.len(), 1);
    assert_eq!(desc.fields[0].name, "kind");
}

#[test]
fn skipped_fields_still_round_trip_as_their_default() {
    // Reading reconstructs the whole struct, so a skipped field has to come from somewhere.
    // It is not replicated, so it comes back as whatever the arena had: zero.
    let mut world = World::new();
    let tags = world.register_component::<Tag>().unwrap();
    let e = world.spawn();
    tags.write(
        &mut world,
        e,
        &Tag {
            kind: 7,
            local_only: 999,
        },
    )
    .unwrap();
    let back = tags.read(&world, e).unwrap();
    assert_eq!(back.kind, 7);
}

#[test]
fn declaration_order_does_not_change_the_schema_id() {
    // The property that lets six languages declare the same component however they like.
    #[derive(Replicate)]
    struct Reordered {
        alive: bool,
        #[replicate(bits = 10)]
        health: u32,
        #[replicate(quantize = "0.01", min = "-50", max = "50")]
        velocity: Vec2,
        #[replicate(quantize = "0.001", min = "-1000", max = "1000")]
        position: Vec2,
    }

    let mut a = tempo_wire::Schema::new();
    a.register(Body::describe()).unwrap();
    let mut b = tempo_wire::Schema::new();
    let mut renamed = Reordered::describe();
    renamed.name = "Body".into();
    b.register(renamed).unwrap();
    assert_eq!(a.schema_id(), b.schema_id());
}

#[test]
fn a_typed_handle_reads_and_writes_the_right_component() {
    let mut world = World::new();
    let bodies = world.register_component::<Body>().unwrap();
    let tags = world.register_component::<Tag>().unwrap();

    let e = world.spawn();
    tags.write(
        &mut world,
        e,
        &Tag {
            kind: 3,
            local_only: 0,
        },
    )
    .unwrap();

    assert!(tags.present(&world, e));
    assert!(!bodies.present(&world, e));
    assert_eq!(bodies.try_read(&world, e), None);
    assert_eq!(tags.try_read(&world, e).unwrap().kind, 3);
}

#[test]
fn derived_components_replicate_end_to_end() {
    let build = || {
        let mut w = World::new();
        let h = w.register_component::<Body>().unwrap();
        (w, h)
    };
    let (mut server, bodies) = build();
    let (mut client, _) = build();

    let e = server.spawn();
    bodies
        .write(
            &mut server,
            e,
            &Body {
                position: Vec2::from_ints(10, 20),
                velocity: Vec2::ZERO,
                health: 100,
                alive: true,
            },
        )
        .unwrap();

    let delta = encode_delta(&server, None).unwrap();
    apply_delta(&mut client, &delta.bytes).unwrap();

    let received = bodies.read(&client, e).unwrap();
    assert_eq!(received.health, 100);
    assert!(received.alive);
    assert!(received.position.sub(Vec2::from_ints(10, 20)).length() < Fx::from_raw(0x0041_8937));
}
