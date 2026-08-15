//! Client prediction and server reconciliation.
//!
//! Implements [ADR-0012](../../../docs/adr/0012-prediction-and-reconciliation.md).
//!
//! A client that waits for the server before showing its own movement has the round-trip time as
//! input latency — 80 ms is sluggish, 100 ms is broken. So the client applies local input
//! immediately and corrects afterwards when the authoritative answer arrives.
//!
//! # The three parts
//!
//! 1. **Predict.** Apply local input at once, and record `(tick, input, resulting state)`.
//! 2. **Reconcile.** When the server's state for tick *T* arrives, compare it against what was
//!    predicted for *T*. Matching is the overwhelmingly common case and costs a comparison.
//!    Mismatching means restoring to the server's state and re-simulating the buffered inputs.
//! 3. **Smooth.** Simulation state is corrected instantly; the *rendered* position blends toward it,
//!    so the player sees an adjustment rather than a teleport.
//!
//! Reconciliation is rollback with a single authoritative source, and it uses the same snapshot
//! machinery — which is why the core owns state at all.

use std::collections::VecDeque;

use tempo_core::{CoreError, StateComparison, Tick, World, WorldSnapshot};
use tempo_fixed::Fx;

/// One predicted tick, kept until the server confirms it.
#[derive(Debug, Clone)]
pub struct PredictedTick<I> {
    /// The tick predicted.
    pub tick: Tick,
    /// The input applied at that tick.
    pub input: I,
    /// State after applying it.
    pub state: WorldSnapshot,
}

/// What reconciliation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The prediction matched; history before the tick was discarded.
    Confirmed {
        /// The tick confirmed.
        tick: Tick,
    },
    /// The prediction was wrong; state was restored and inputs replayed.
    Corrected {
        /// The tick the correction applied to.
        tick: Tick,
        /// How many ticks were re-simulated.
        resimulated: usize,
        /// Which field first disagreed, for diagnostics.
        cause: StateComparison,
    },
    /// The snapshot described a tick already confirmed and released. Ignored.
    ///
    /// This is **not** a failure and must not be treated as one. Snapshots are duplicated by the
    /// network and delayed by latency, so old news arrives routinely. Treating it as a resync would
    /// throw away good prediction history and — because the next snapshot would then also be older
    /// than everything held — cascade into resyncing forever. That cascade is exactly what the
    /// end-to-end demo hit on a 40 ms link before this variant existed.
    Stale {
        /// The tick the snapshot described.
        tick: Tick,
        /// The oldest tick still held.
        oldest_held: Tick,
    },
    /// The snapshot was for a tick ahead of anything predicted, so there is nothing to reconcile.
    ///
    /// Means the client has fallen behind the server rather than running ahead of it. The state is
    /// adopted wholesale and prediction restarts.
    Resynced {
        /// The tick adopted.
        tick: Tick,
    },
}

/// Keeps predicted history and reconciles it against authoritative snapshots.
pub struct Predictor<I> {
    history: VecDeque<PredictedTick<I>>,
    max_history: usize,
    tolerance: Fx,
    resimulated_total: u64,
    corrections: u64,
}

impl<I: Clone> Predictor<I> {
    /// Creates a predictor holding `max_history` ticks of prediction.
    ///
    /// History must cover the worst round trip the game intends to support, or the server's answer
    /// will routinely arrive after the tick it refers to has been discarded, forcing a full resync
    /// instead of a reconciliation.
    pub fn new(max_history: usize, tolerance: Fx) -> Predictor<I> {
        assert!(
            max_history > 0,
            "prediction needs at least one tick of history"
        );
        Predictor {
            history: VecDeque::with_capacity(max_history),
            max_history,
            tolerance,
            resimulated_total: 0,
            corrections: 0,
        }
    }

    /// Records a predicted tick.
    pub fn record(&mut self, tick: Tick, input: I, state: WorldSnapshot) {
        if self.history.len() == self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(PredictedTick { tick, input, state });
    }

    /// Ticks currently held.
    #[inline]
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// True if nothing is predicted.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// The oldest tick still held.
    pub fn oldest(&self) -> Option<Tick> {
        self.history.front().map(|p| p.tick)
    }

    /// The newest tick held.
    pub fn newest(&self) -> Option<Tick> {
        self.history.back().map(|p| p.tick)
    }

    /// Total ticks re-simulated across all corrections. A cost metric.
    #[inline]
    pub fn resimulated_total(&self) -> u64 {
        self.resimulated_total
    }

    /// Number of corrections applied. Compare against ticks predicted to judge prediction quality.
    #[inline]
    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// Discards all history, for a hard resync.
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// Reconciles the world against an authoritative snapshot.
    ///
    /// `simulate` advances the world by one tick under an input. It must be the *same* function the
    /// prediction used, and it must be deterministic — if it is not, every tick will appear to
    /// diverge and the client will re-simulate constantly.
    pub fn reconcile<F>(
        &mut self,
        world: &mut World,
        authoritative: &WorldSnapshot,
        mut simulate: F,
    ) -> Result<Reconciliation, CoreError>
    where
        F: FnMut(&mut World, &I),
    {
        let tick = authoritative.tick;

        let Some(pos) = self.history.iter().position(|p| p.tick == tick) else {
            // Distinguish "old news" from "we are behind". Conflating them is a real bug: an
            // already-confirmed tick arriving late would clear history, after which every
            // subsequent snapshot is also older than everything held, and the client resyncs
            // forever without ever predicting again.
            if let Some(oldest) = self.oldest() {
                if oldest.is_newer_than(tick) {
                    return Ok(Reconciliation::Stale {
                        tick,
                        oldest_held: oldest,
                    });
                }
            }
            world.restore(authoritative)?;
            self.history.clear();
            return Ok(Reconciliation::Resynced { tick });
        };

        let comparison = {
            let predicted = &self.history[pos];
            let mut scratch = world.clone();
            scratch.restore(&predicted.state)?;
            scratch.compare_with(authoritative, self.tolerance)
        };

        if comparison == StateComparison::Agrees {
            // The common case. Everything up to and including this tick is confirmed.
            self.history.drain(..=pos);
            return Ok(Reconciliation::Confirmed { tick });
        }

        // Restore to the authority's state and replay everything after it.
        world.restore(authoritative)?;
        let replay: Vec<I> = self
            .history
            .iter()
            .skip(pos + 1)
            .map(|p| p.input.clone())
            .collect();
        let ticks: Vec<Tick> = self.history.iter().skip(pos + 1).map(|p| p.tick).collect();

        let mut rebuilt: VecDeque<PredictedTick<I>> = VecDeque::with_capacity(replay.len());
        for (input, t) in replay.into_iter().zip(ticks) {
            simulate(world, &input);
            world.set_tick(t);
            rebuilt.push_back(PredictedTick {
                tick: t,
                input,
                state: world.snapshot(),
            });
        }

        let resimulated = rebuilt.len();
        self.history = rebuilt;
        self.resimulated_total += resimulated as u64;
        self.corrections += 1;

        Ok(Reconciliation::Corrected {
            tick,
            resimulated,
            cause: comparison,
        })
    }
}

impl<I> core::fmt::Debug for Predictor<I> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Predictor")
            .field("held", &self.history.len())
            .field("max_history", &self.max_history)
            .field("corrections", &self.corrections)
            .field("resimulated_total", &self.resimulated_total)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_core::ComponentId;
    use tempo_fixed::Vec2;
    use tempo_wire::{ComponentDesc, FieldDesc, FieldType, Value};

    const STEP: Fx = Fx::from_raw(0x0041_8937); // 0.001

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Input {
        dx: i32,
    }

    fn setup() -> (World, ComponentId) {
        let mut w = World::new();
        let c = w
            .register(ComponentDesc::new(
                "Body",
                vec![FieldDesc::new("position", FieldType::Vec2).with_quantize(
                    STEP,
                    Fx::from_int(-1000),
                    Fx::from_int(1000),
                )],
            ))
            .unwrap();
        (w, c)
    }

    /// Moves the entity by the input, in the same way on client and server.
    fn simulate(world: &mut World, c: ComponentId, input: &Input) {
        let entities: Vec<_> = world.entities().collect();
        for e in entities {
            if let Ok(Value::Vec2(p)) = world.get_named(e, c, "position") {
                let next = Vec2::new(p.x.add(Fx::from_ratio(input.dx, 10)), p.y);
                let _ = world.set_named(e, c, "position", &Value::Vec2(next));
            }
        }
    }

    #[test]
    fn a_matching_prediction_confirms_and_frees_history() {
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(64, STEP);
        for t in 1..=5u32 {
            simulate(&mut world, c, &Input { dx: 1 });
            world.set_tick(Tick(t));
            p.record(Tick(t), Input { dx: 1 }, world.snapshot());
        }
        assert_eq!(p.len(), 5);

        // The server agrees about tick 3.
        let authoritative = p.history[2].state.clone();
        let outcome = p
            .reconcile(&mut world, &authoritative, |w, i| simulate(w, c, i))
            .unwrap();

        assert_eq!(outcome, Reconciliation::Confirmed { tick: Tick(3) });
        assert_eq!(p.len(), 2, "confirmed ticks are discarded");
        assert_eq!(p.corrections(), 0);
    }

    #[test]
    fn a_wrong_prediction_restores_and_replays() {
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(64, STEP);
        for t in 1..=5u32 {
            simulate(&mut world, c, &Input { dx: 1 });
            world.set_tick(Tick(t));
            p.record(Tick(t), Input { dx: 1 }, world.snapshot());
        }

        // The server says the entity was somewhere else entirely at tick 2 — a collision the
        // client could not have known about.
        let mut corrected = world.clone();
        corrected.restore(&p.history[1].state).unwrap();
        corrected
            .set_named(e, c, "position", &Value::Vec2(Vec2::from_ints(50, 0)))
            .unwrap();
        let authoritative = corrected.snapshot();

        let outcome = p
            .reconcile(&mut world, &authoritative, |w, i| simulate(w, c, i))
            .unwrap();

        match outcome {
            Reconciliation::Corrected {
                tick, resimulated, ..
            } => {
                assert_eq!(tick, Tick(2));
                assert_eq!(resimulated, 3, "ticks 3, 4 and 5 are replayed");
            }
            other => panic!("expected a correction, got {other:?}"),
        }

        // The replayed inputs must have been applied on top of the server's state: 50 plus three
        // more steps of 0.1.
        let Value::Vec2(pos) = world.get_named(e, c, "position").unwrap() else {
            unreachable!()
        };
        let expected = Fx::from_int(50).add(Fx::from_ratio(3, 10));
        assert!(pos.x.sub(expected).abs().raw() <= STEP.raw() * 2, "{pos:?}");
        assert_eq!(world.tick(), Tick(5), "the world is back at the present");
        assert_eq!(p.corrections(), 1);
        assert_eq!(p.resimulated_total(), 3);
    }

    #[test]
    fn quantization_alone_does_not_trigger_corrections() {
        // The property that makes prediction usable at all. A client predicts in raw fixed point
        // while the server's snapshot has been quantized, so the two always differ slightly. If
        // that counted as divergence the client would re-simulate every single tick.
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(
                e,
                c,
                "position",
                &Value::Vec2(Vec2::new(Fx::from_ratio(1, 3), Fx::ZERO)),
            )
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(64, STEP);
        simulate(&mut world, c, &Input { dx: 1 });
        world.set_tick(Tick(1));
        p.record(Tick(1), Input { dx: 1 }, world.snapshot());

        // Build the server's view by round-tripping through quantization.
        let mut server_view = world.clone();
        let Value::Vec2(raw) = world.get_named(e, c, "position").unwrap() else {
            unreachable!()
        };
        let q = |v: Fx| {
            tempo_wire::dequantize(
                tempo_wire::quantize_value(v, Fx::from_int(-1000), Fx::from_int(1000), STEP),
                Fx::from_int(-1000),
                STEP,
            )
        };
        server_view
            .set_named(
                e,
                c,
                "position",
                &Value::Vec2(Vec2::new(q(raw.x), q(raw.y))),
            )
            .unwrap();

        let outcome = p
            .reconcile(&mut world, &server_view.snapshot(), |w, i| {
                simulate(w, c, i)
            })
            .unwrap();
        assert_eq!(outcome, Reconciliation::Confirmed { tick: Tick(1) });
        assert_eq!(p.corrections(), 0);
    }

    #[test]
    fn an_already_confirmed_tick_is_stale_not_a_resync() {
        // Conflating the two cascades: clearing history means the next snapshot is also older than
        // everything held, and the client never predicts again.
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(64, STEP);
        for t in 1..=5u32 {
            simulate(&mut world, c, &Input { dx: 1 });
            world.set_tick(Tick(t));
            p.record(Tick(t), Input { dx: 1 }, world.snapshot());
        }

        let snap_at_3 = p.history[2].state.clone();
        assert_eq!(
            p.reconcile(&mut world, &snap_at_3, |w, i| simulate(w, c, i))
                .unwrap(),
            Reconciliation::Confirmed { tick: Tick(3) }
        );

        // The same snapshot arrives again, duplicated by the network.
        let outcome = p
            .reconcile(&mut world, &snap_at_3, |w, i| simulate(w, c, i))
            .unwrap();
        assert_eq!(
            outcome,
            Reconciliation::Stale {
                tick: Tick(3),
                oldest_held: Tick(4)
            }
        );
        assert_eq!(p.len(), 2, "history must survive old news");
    }

    #[test]
    fn a_snapshot_for_a_forgotten_tick_forces_a_resync() {
        // Ordinary on a bad connection: the answer arrives after the tick was discarded.
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(4, STEP);
        for t in 1..=10u32 {
            simulate(&mut world, c, &Input { dx: 1 });
            world.set_tick(Tick(t));
            p.record(Tick(t), Input { dx: 1 }, world.snapshot());
        }
        assert_eq!(p.len(), 4, "history is bounded");
        assert_eq!(p.oldest(), Some(Tick(7)));

        // A tick ahead of everything predicted: the client has fallen behind and must adopt.
        let mut ahead = world.clone();
        ahead.set_tick(Tick(500));
        let outcome = p
            .reconcile(&mut world, &ahead.snapshot(), |w, i| simulate(w, c, i))
            .unwrap();
        assert_eq!(outcome, Reconciliation::Resynced { tick: Tick(500) });
        assert!(p.is_empty());
    }

    #[test]
    fn history_is_bounded_so_a_stalled_server_cannot_grow_it() {
        let (mut world, c) = setup();
        let e = world.spawn();
        world
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();

        let mut p: Predictor<Input> = Predictor::new(8, STEP);
        for t in 1..=1000u32 {
            simulate(&mut world, c, &Input { dx: 1 });
            world.set_tick(Tick(t));
            p.record(Tick(t), Input { dx: 1 }, world.snapshot());
        }
        assert_eq!(p.len(), 8);
        assert_eq!(p.newest(), Some(Tick(1000)));
    }

    #[test]
    fn repeated_reconciliation_converges_rather_than_drifting() {
        // A client and server running the same deterministic simulation should confirm every tick
        // and never accumulate error.
        let (mut client, c) = setup();
        let e = client.spawn();
        client
            .set_named(e, c, "position", &Value::Vec2(Vec2::ZERO))
            .unwrap();
        let mut server = client.clone();

        let mut p: Predictor<Input> = Predictor::new(64, STEP);
        for t in 1..=200u32 {
            let input = Input {
                dx: (t % 5) as i32 - 2,
            };

            simulate(&mut client, c, &input);
            client.set_tick(Tick(t));
            p.record(Tick(t), input, client.snapshot());

            simulate(&mut server, c, &input);
            server.set_tick(Tick(t));

            let outcome = p
                .reconcile(&mut client, &server.snapshot(), |w, i| simulate(w, c, i))
                .unwrap();
            assert_eq!(
                outcome,
                Reconciliation::Confirmed { tick: Tick(t) },
                "at tick {t}"
            );
        }
        assert_eq!(
            p.corrections(),
            0,
            "an agreeing simulation must never correct"
        );
        assert!(p.is_empty(), "every tick was confirmed and released");
    }
}
