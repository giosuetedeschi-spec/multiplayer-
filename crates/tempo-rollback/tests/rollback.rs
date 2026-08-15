//! Rollback behaviour, tested against a deterministic toy simulation.
//!
//! The central property is stated directly in
//! [`resimulation_reproduces_a_straight_run`]: simulating ticks 0..n directly, and simulating
//! 0..k then rolling back to j and re-simulating to n, must produce identical state. Everything
//! else in the crate exists to make that hold under real conditions.

use tempo_core::{ComponentId, Entity, Tick, World};
use tempo_fixed::{Fx, Vec2};
use tempo_rollback::{
    InputSource, PlayerId, RollbackConfig, RollbackError, RollbackLimit, RollbackSession,
};
use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

const P1: PlayerId = PlayerId(0);
const P2: PlayerId = PlayerId(1);

/// A player's input: a thrust direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Move {
    dx: i8,
    dy: i8,
}

fn build() -> (World, ComponentId, Vec<Entity>) {
    let mut w = World::new();
    let body = w
        .register(ComponentDesc::new(
            "Body",
            vec![FieldDesc::new("position", FieldType::Vec2).with_quantize(
                Fx::from_raw(0x0041_8937),
                Fx::from_int(-1000),
                Fx::from_int(1000),
            )],
        ))
        .unwrap();
    let entities: Vec<Entity> = (0..2)
        .map(|_| {
            let e = w.spawn();
            w.set_named(e, body, "position", &Value::Vec2(Vec2::ZERO))
                .unwrap();
            e
        })
        .collect();
    (w, body, entities)
}

/// Applies a frame of inputs. Deterministic, order-independent per player index.
fn simulate(
    world: &mut World,
    body: ComponentId,
    entities: &[Entity],
    _tick: Tick,
    frame: &[(PlayerId, Move, InputSource)],
) {
    for (player, mv, _) in frame {
        let Some(&e) = entities.get(player.0 as usize) else {
            continue;
        };
        let Ok(Value::Vec2(p)) = world.get_named(e, body, "position") else {
            continue;
        };
        let next = Vec2::new(
            p.x.add(Fx::from_ratio(mv.dx as i32, 10)),
            p.y.add(Fx::from_ratio(mv.dy as i32, 10)),
        );
        let _ = world.set_named(e, body, "position", &Value::Vec2(next));
    }
}

fn session() -> RollbackSession<Move> {
    let mut s = RollbackSession::new(RollbackConfig {
        max_rollback: 8,
        input_delay: 0,
        input_history: 128,
    });
    s.add_player(P1);
    s.add_player(P2);
    s
}

#[test]
fn resimulation_reproduces_a_straight_run() {
    // The property the whole design rests on. If this fails, nothing above it can work.
    let (mut a, body, entities) = build();
    let mut sa = session();
    for t in 1..=20u32 {
        sa.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        sa.inputs().confirm(P2, Tick(t), Move { dx: 0, dy: 1 });
        sa.advance(&mut a, |w, tick, f| simulate(w, body, &entities, tick, f))
            .unwrap();
    }
    let straight_through = a.state_hash();

    let (mut b, body2, entities2) = build();
    let mut sb = session();
    for t in 1..=20u32 {
        sb.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        sb.inputs().confirm(P2, Tick(t), Move { dx: 0, dy: 1 });
        sb.advance(&mut b, |w, tick, f| simulate(w, body2, &entities2, tick, f))
            .unwrap();
    }
    // Roll back to tick 13 and replay to 20.
    sb.rollback_to(&mut b, Tick(13), |w, tick, f| {
        simulate(w, body2, &entities2, tick, f)
    })
    .unwrap();

    assert_eq!(b.state_hash(), straight_through, "re-simulation diverged");
    assert_eq!(
        sb.current_tick(),
        Tick(20),
        "the world is back at the present"
    );
}

#[test]
fn a_contradicted_prediction_triggers_a_rollback_and_corrects_the_result() {
    let (mut world, body, entities) = build();
    let mut s = session();

    // P1's inputs are known; P2 is silent, so its input is predicted as "no movement".
    for t in 1..=5u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }
    let Value::Vec2(before) = world.get_named(entities[1], body, "position").unwrap() else {
        unreachable!()
    };
    assert_eq!(before.y, Fx::ZERO, "P2 was predicted as stationary");

    // P2's real input for tick 2 arrives, and it moved.
    let rolled = s
        .add_remote_input(
            &mut world,
            P2,
            Tick(2),
            Move { dx: 0, dy: 1 },
            |w, tick, f| simulate(w, body, &entities, tick, f),
        )
        .unwrap();
    assert_eq!(rolled, Some(Tick(2)));

    let Value::Vec2(after) = world.get_named(entities[1], body, "position").unwrap() else {
        unreachable!()
    };
    // Tick 2 moved, and ticks 3-5 repeat it as the new prediction: four steps of 0.1.
    assert!(
        after.y.sub(Fx::from_ratio(4, 10)).abs().raw() < Fx::from_raw(0x0041_8937).raw() * 2,
        "corrected position was {after:?}"
    );
    assert_eq!(s.stats().rollbacks, 1);
    assert_eq!(s.stats().resimulated, 4, "ticks 2 through 5");
}

#[test]
fn a_confirmation_matching_the_prediction_costs_nothing() {
    // The common case. Repeat-last is right most of the time, which is what makes rollback cheap.
    let (mut world, body, entities) = build();
    let mut s = session();

    s.inputs().confirm(P2, Tick(1), Move { dx: 0, dy: 1 });
    for t in 1..=4u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }

    // P2 kept holding the same direction, exactly as predicted.
    let rolled = s
        .add_remote_input(
            &mut world,
            P2,
            Tick(3),
            Move { dx: 0, dy: 1 },
            |w, tick, f| simulate(w, body, &entities, tick, f),
        )
        .unwrap();
    assert_eq!(rolled, None);
    assert_eq!(s.stats().rollbacks, 0);
}

#[test]
fn rollbacks_beyond_the_window_are_refused_rather_than_attempted() {
    // Worst-case frame time is window times tick cost. An unbounded window means a peer that
    // vanished for two seconds causes a visible freeze; stalling is more predictable.
    let (mut world, body, entities) = build();
    let mut s = RollbackSession::new(RollbackConfig {
        max_rollback: 4,
        input_delay: 0,
        input_history: 128,
    });
    s.add_player(P1);

    for t in 1..=20u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }

    let err = s.rollback_to(&mut world, Tick(2), |w, tick, f| {
        simulate(w, body, &entities, tick, f)
    });
    assert!(matches!(
        err,
        Err(RollbackError::Limit(RollbackLimit::BeyondWindow { .. }))
    ));
    assert_eq!(s.stats().refused, 1);
    assert_eq!(
        s.current_tick(),
        Tick(20),
        "a refused rollback changes nothing"
    );
}

#[test]
fn a_rollback_at_the_window_edge_still_works() {
    let (mut world, body, entities) = build();
    let mut s = RollbackSession::new(RollbackConfig {
        max_rollback: 4,
        input_delay: 0,
        input_history: 128,
    });
    s.add_player(P1);
    for t in 1..=20u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }
    let replayed = s
        .rollback_to(&mut world, Tick(17), |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .expect("exactly at the window edge");
    assert_eq!(replayed, 4);
}

#[test]
fn sync_test_passes_on_a_deterministic_simulation() {
    let (mut world, body, entities) = build();
    let mut s = session();
    for t in 1..=10u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.inputs().confirm(P2, Tick(t), Move { dx: 0, dy: 1 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
        s.sync_test(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .expect("the toy simulation is deterministic");
    }
}

#[test]
fn sync_test_catches_a_nondeterministic_simulation() {
    // The point of the mode. A simulation that reads ambient state produces a different result on
    // re-simulation, and this must be caught on a developer's machine rather than as a desync in
    // someone's match.
    let (mut world, body, entities) = build();
    let mut s = session();
    let mut hidden_counter = 0i32;

    let mut naughty = |w: &mut World, _t: Tick, _f: &[(PlayerId, Move, InputSource)]| {
        // Depends on how many times it has been called — exactly what re-simulation breaks.
        hidden_counter += 1;
        let Ok(Value::Vec2(p)) = w.get_named(entities[0], body, "position") else {
            return;
        };
        let next = Vec2::new(p.x.add(Fx::from_ratio(hidden_counter, 100)), p.y);
        let _ = w.set_named(entities[0], body, "position", &Value::Vec2(next));
    };

    s.inputs().confirm(P1, Tick(1), Move::default());
    s.inputs().confirm(P2, Tick(1), Move::default());
    s.advance(&mut world, &mut naughty).unwrap();

    let result = s.sync_test(&mut world, &mut naughty);
    assert!(
        matches!(
            result,
            Err(RollbackError::Nondeterministic { tick: Tick(1) })
        ),
        "expected a nondeterminism report, got {result:?}"
    );
}

#[test]
fn input_delay_shifts_local_input_into_the_future() {
    // The classic trade: uniform latency in exchange for fewer visible corrections.
    let (mut world, body, entities) = build();
    let mut s = RollbackSession::new(RollbackConfig {
        max_rollback: 8,
        input_delay: 3,
        input_history: 128,
    });
    s.add_player(P1);

    let target = s.add_local_input(P1, Move { dx: 1, dy: 0 });
    assert_eq!(target, Tick(4), "current 0, plus 3 delay, plus 1");
    assert!(s.inputs().is_confirmed(P1, Tick(4)));
    assert!(!s.inputs().is_confirmed(P1, Tick(1)));

    let _ = s.advance(&mut world, |w, tick, f| {
        simulate(w, body, &entities, tick, f)
    });
}

#[test]
fn the_confirmed_frame_advances_with_the_slowest_peer() {
    let (mut world, body, entities) = build();
    let mut s = session();
    assert_eq!(s.confirmed_frame(), None);

    for t in 1..=5u32 {
        s.inputs().confirm(P1, Tick(t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }
    assert_eq!(s.confirmed_frame(), None, "P2 has confirmed nothing");

    s.inputs().confirm(P2, Tick(3), Move::default());
    assert_eq!(s.confirmed_frame(), Some(Tick(3)));
}

#[test]
fn many_rollbacks_leave_the_world_consistent() {
    // Sustained adversarial conditions: remote inputs arriving late and contradicting predictions
    // over and over. Errors that only accumulate show up here.
    let (mut reference, rbody, rentities) = build();
    let mut plain = session();
    let script: Vec<(u32, Move)> = (1..=60u32)
        .map(|t| {
            (
                t,
                Move {
                    dx: ((t % 3) as i8) - 1,
                    dy: ((t % 5) as i8) - 2,
                },
            )
        })
        .collect();

    // A straight run with every input known in advance.
    for (t, mv) in &script {
        plain.inputs().confirm(P1, Tick(*t), Move { dx: 1, dy: 0 });
        plain.inputs().confirm(P2, Tick(*t), *mv);
        plain
            .advance(&mut reference, |w, tick, f| {
                simulate(w, rbody, &rentities, tick, f)
            })
            .unwrap();
    }
    let expected = reference.state_hash();

    // The same session, but P2's inputs arrive three ticks late and keep contradicting.
    let (mut world, body, entities) = build();
    let mut s = session();
    for (t, _mv) in &script {
        s.inputs().confirm(P1, Tick(*t), Move { dx: 1, dy: 0 });
        s.advance(&mut world, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();

        if *t > 3 {
            let late = t - 3;
            let (_, late_move) = script[(late - 1) as usize];
            s.add_remote_input(&mut world, P2, Tick(late), late_move, |w, tick, f| {
                simulate(w, body, &entities, tick, f)
            })
            .unwrap();
        }
    }
    // Deliver the final few that were still outstanding.
    for late in 58..=60u32 {
        let (_, late_move) = script[(late - 1) as usize];
        s.add_remote_input(&mut world, P2, Tick(late), late_move, |w, tick, f| {
            simulate(w, body, &entities, tick, f)
        })
        .unwrap();
    }

    assert_eq!(world.state_hash(), expected, "late inputs did not converge");
    assert!(
        s.stats().rollbacks > 10,
        "expected many rollbacks, got {}",
        s.stats().rollbacks
    );
    assert!(
        s.stats().deepest_rollback <= s.config().max_rollback,
        "a rollback exceeded the configured window"
    );
}
