//! Priority accumulation: choosing *which* relevant entities to send this tick.
//!
//! Filtering by relevance is not enough. A client in a crowded area may have 800 relevant entities
//! and budget for 150 updates, so something must choose — and must choose *differently* next tick,
//! or the same subset wins forever and everything else is invisible.
//!
//! # Why accumulation rather than the obvious alternatives
//!
//! **Round-robin** guarantees eventual delivery with no bookkeeping, and treats a duelling opponent
//! exactly like a distant crate. It spends scarce bandwidth on the wrong things precisely when
//! bandwidth is scarce.
//!
//! **Strict distance ordering** is intuitive and starves everything past the cutoff *permanently*,
//! so a distant sniper or a moving objective is simply never updated.
//!
//! Accumulation gets both properties at once. Each `(entity, observer)` pair builds up priority
//! every tick it is skipped, so a low-priority entity eventually outranks the busy ones and nothing
//! starves. The system degrades into "important things update at full rate, unimportant things
//! update slowly" rather than "unimportant things never update", which is the difference between a
//! game that feels congested and one that feels broken.

use std::collections::HashMap;

use tempo_core::Entity;
use tempo_fixed::Fx;

/// One `(observer, entity)` pair's accumulation state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Entry {
    score: Fx,
    ticks_since_sent: u32,
}

/// Per-observer, per-entity accumulated priority.
#[derive(Debug)]
pub struct PriorityAccumulator {
    /// Keyed by `(observer, entity)`. Stored flat rather than nested: the replication sweep walks
    /// it once per observer and a nested map would allocate per observer per tick.
    entries: HashMap<(Entity, Entity), Entry>,
    staleness_scale: Fx,
}

impl Default for PriorityAccumulator {
    fn default() -> PriorityAccumulator {
        PriorityAccumulator::new()
    }
}

/// What to send an observer this tick, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Entities to include, highest priority first.
    pub entities: Vec<Entity>,
    /// Bytes the selection is expected to occupy.
    pub bytes: usize,
    /// Relevant entities that did not fit.
    pub deferred: usize,
}

impl PriorityAccumulator {
    /// Creates an accumulator with a one-second staleness scale at 60 Hz.
    pub fn new() -> PriorityAccumulator {
        PriorityAccumulator::with_staleness_scale(Fx::from_int(60))
    }

    /// Creates an accumulator with an explicit staleness scale, in ticks.
    ///
    /// Smaller values make neglected entities catch up faster, at the cost of respecting declared
    /// priority less. See [`PriorityAccumulator::accumulate`].
    pub fn with_staleness_scale(ticks: Fx) -> PriorityAccumulator {
        assert!(ticks.raw() > 0, "the staleness scale must be positive");
        PriorityAccumulator {
            entries: HashMap::new(),
            staleness_scale: ticks,
        }
    }

    /// Adds priority for one observer, scaled by how long the entity has gone unsent.
    ///
    /// ADR-0011 specifies the gain as `base × distance × staleness`, and the staleness term is not
    /// decoration. With a constant gain, the time for a low-priority entity to win a slot scales
    /// *linearly with the priority ratio*: at 100:1 under contention it waits roughly six hundred
    /// ticks, or ten seconds at 60 Hz. Measured, not estimated — the first implementation here used
    /// a constant gain and the starvation test caught it.
    ///
    /// Growing the gain with age makes the wait scale with the square root of the ratio instead, so
    /// the same entity arrives in a few seconds. Priority still decides the common case; staleness
    /// decides the tail.
    pub fn accumulate(&mut self, observer: Entity, entity: Entity, amount: Fx) {
        let scale = self.staleness_scale;
        let slot = self.entries.entry((observer, entity)).or_default();
        slot.ticks_since_sent = slot.ticks_since_sent.saturating_add(1);

        let staleness =
            Fx::ONE.add(Fx::from_int(slot.ticks_since_sent.min(i32::MAX as u32) as i32).div(scale));
        slot.score = slot.score.add(amount.mul(staleness));
    }

    /// The current priority of an entity for an observer.
    pub fn score(&self, observer: Entity, entity: Entity) -> Fx {
        self.entries
            .get(&(observer, entity))
            .map(|e| e.score)
            .unwrap_or(Fx::ZERO)
    }

    /// Ticks since an entity was last sent to an observer.
    pub fn ticks_since_sent(&self, observer: Entity, entity: Entity) -> u32 {
        self.entries
            .get(&(observer, entity))
            .map(|e| e.ticks_since_sent)
            .unwrap_or(0)
    }

    /// Resets an entity's priority and staleness, after sending it.
    pub fn reset(&mut self, observer: Entity, entity: Entity) {
        self.entries.insert((observer, entity), Entry::default());
    }

    /// Forgets an entity for one observer, when it stops being relevant.
    pub fn forget(&mut self, observer: Entity, entity: Entity) {
        self.entries.remove(&(observer, entity));
    }

    /// Forgets everything for an observer, when a client disconnects.
    ///
    /// Without this the map grows for the life of the process, since entries are keyed by observer.
    pub fn remove_observer(&mut self, observer: Entity) {
        self.entries.retain(|(o, _), _| *o != observer);
    }

    /// Number of tracked pairs. The dominant allocation in a large session.
    #[inline]
    pub fn tracked_pairs(&self) -> usize {
        self.entries.len()
    }

    /// Chooses what to send, highest priority first, until the budget is exhausted.
    ///
    /// `candidates` pairs each relevant entity with its expected encoded size. Entities that are
    /// sent have their priority reset; entities that are skipped keep accumulating, which is what
    /// prevents starvation.
    ///
    /// Ties break on entity index, so a run is reproducible — sorting an equal-priority set by
    /// whatever order the map yielded would make two identical states replicate differently.
    pub fn select(
        &mut self,
        observer: Entity,
        candidates: &[(Entity, usize)],
        budget_bytes: usize,
    ) -> Selection {
        let mut ranked: Vec<(Entity, usize, Fx)> = candidates
            .iter()
            .map(|(e, size)| (*e, *size, self.score(observer, *e)))
            .collect();

        ranked.sort_by(|a, b| {
            b.2.raw()
                .cmp(&a.2.raw())
                .then_with(|| a.0.index().cmp(&b.0.index()))
        });

        let mut chosen = Vec::new();
        let mut used = 0usize;
        let mut deferred = 0usize;

        for (entity, size, _) in ranked {
            if used + size <= budget_bytes {
                used += size;
                chosen.push(entity);
                self.reset(observer, entity);
            } else {
                // Skipped, not dropped: its priority keeps growing and it will win a later tick.
                deferred += 1;
            }
        }

        Selection {
            entities: chosen,
            bytes: used,
            deferred,
        }
    }
}

/// Computes how much priority an entity gains per tick for an observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriorityRule {
    /// Multiplier from the component's schema.
    pub base: Fx,
    /// Distance at which priority is halved.
    ///
    /// Nearby entities accumulate faster and so update more often, without the hard cutoff that
    /// makes distant things invisible.
    pub half_distance: Fx,
}

impl Default for PriorityRule {
    fn default() -> PriorityRule {
        PriorityRule {
            base: Fx::ONE,
            half_distance: Fx::from_int(50),
        }
    }
}

impl PriorityRule {
    /// Priority gained this tick at a given distance.
    ///
    /// Falls off as `base / (1 + distance / half_distance)`, which is cheap, monotonic, and never
    /// reaches zero — an entity at any distance still accumulates, just slowly. A rule that could
    /// reach zero would reintroduce permanent starvation through the back door.
    pub fn gain(&self, distance: Fx) -> Fx {
        let d = distance.max(Fx::ZERO);
        let denom = Fx::ONE.add(d.div(self.half_distance));
        self.base.div(denom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: u32) -> Entity {
        Entity::from_parts(i, 0)
    }

    const OBS: Entity = Entity::from_parts(1000, 0);

    #[test]
    fn the_highest_priority_entities_are_sent_first() {
        let mut a = PriorityAccumulator::new();
        a.accumulate(OBS, e(1), Fx::from_int(1));
        a.accumulate(OBS, e(2), Fx::from_int(5));
        a.accumulate(OBS, e(3), Fx::from_int(3));

        let s = a.select(OBS, &[(e(1), 10), (e(2), 10), (e(3), 10)], 100);
        assert_eq!(s.entities, vec![e(2), e(3), e(1)]);
        assert_eq!(s.bytes, 30);
        assert_eq!(s.deferred, 0);
    }

    #[test]
    fn the_budget_is_respected() {
        let mut a = PriorityAccumulator::new();
        for i in 1..=5u32 {
            a.accumulate(OBS, e(i), Fx::from_int(i as i32));
        }
        let candidates: Vec<(Entity, usize)> = (1..=5u32).map(|i| (e(i), 30)).collect();

        let s = a.select(OBS, &candidates, 100);
        assert_eq!(s.entities.len(), 3, "only three fit");
        assert_eq!(s.bytes, 90);
        assert_eq!(s.deferred, 2);
        assert_eq!(s.entities, vec![e(5), e(4), e(3)]);
    }

    #[test]
    fn sending_resets_priority_and_skipping_does_not() {
        let mut a = PriorityAccumulator::new();
        a.accumulate(OBS, e(1), Fx::from_int(10));
        a.accumulate(OBS, e(2), Fx::from_int(1));

        let skipped_before = a.score(OBS, e(2));
        a.select(OBS, &[(e(1), 30), (e(2), 30)], 30);
        assert_eq!(a.score(OBS, e(1)), Fx::ZERO, "sent, so reset");
        assert_eq!(a.score(OBS, e(2)), skipped_before, "skipped, so retained");
        assert!(skipped_before.raw() > 0);
    }

    #[test]
    fn nothing_starves_however_low_its_priority() {
        // The property that makes bounded bandwidth compatible with eventual consistency, and the
        // reason this is not round-robin or a distance cutoff.
        let mut a = PriorityAccumulator::new();
        let candidates: Vec<(Entity, usize)> = (0..20u32).map(|i| (e(i), 10)).collect();

        // Entity 0 is a hundred times less interesting than everything else.
        let mut sent_counts = [0usize; 20];
        for _ in 0..500 {
            for i in 0..20u32 {
                let gain = if i == 0 {
                    Fx::from_ratio(1, 100)
                } else {
                    Fx::ONE
                };
                a.accumulate(OBS, e(i), gain);
            }
            // Budget for only three of the twenty.
            let s = a.select(OBS, &candidates, 30);
            for chosen in s.entities {
                sent_counts[chosen.index() as usize] += 1;
            }
        }

        assert!(
            sent_counts[0] > 0,
            "the least interesting entity was never sent — it starved"
        );
        assert!(
            sent_counts[1] > sent_counts[0] * 5,
            "priority should still matter: {} vs {}",
            sent_counts[1],
            sent_counts[0]
        );
    }

    #[test]
    fn staleness_bounds_the_wait_for_a_neglected_entity() {
        // The reason the gain grows with age. With a constant gain the wait scales linearly with
        // the priority ratio; at 100:1 under contention that is roughly ten seconds at 60 Hz.
        let mut a = PriorityAccumulator::new();
        let candidates: Vec<(Entity, usize)> = (0..20u32).map(|i| (e(i), 10)).collect();

        let mut first_sent_at: Option<usize> = None;
        for tick in 0..600 {
            for i in 0..20u32 {
                let gain = if i == 0 {
                    Fx::from_ratio(1, 100)
                } else {
                    Fx::ONE
                };
                a.accumulate(OBS, e(i), gain);
            }
            let s = a.select(OBS, &candidates, 30);
            if first_sent_at.is_none() && s.entities.contains(&e(0)) {
                first_sent_at = Some(tick);
            }
        }

        let waited = first_sent_at.expect("the neglected entity should eventually be sent");
        assert!(
            waited < 300,
            "waited {waited} ticks (five seconds at 60 Hz); staleness is not accelerating it"
        );
    }

    #[test]
    fn sending_clears_staleness_as_well_as_score() {
        let mut a = PriorityAccumulator::new();
        for _ in 0..50 {
            a.accumulate(OBS, e(1), Fx::ONE);
        }
        assert_eq!(a.ticks_since_sent(OBS, e(1)), 50);

        a.select(OBS, &[(e(1), 10)], 100);
        assert_eq!(a.ticks_since_sent(OBS, e(1)), 0, "staleness must reset too");
        assert_eq!(a.score(OBS, e(1)), Fx::ZERO);
    }

    #[test]
    fn selection_is_reproducible_when_priorities_tie() {
        // Equal scores are common at startup. Without an explicit tiebreak the map's iteration
        // order decides, and two identical states replicate differently.
        let run = || {
            let mut a = PriorityAccumulator::new();
            let candidates: Vec<(Entity, usize)> = (0..50u32).map(|i| (e(i), 10)).collect();
            for i in 0..50u32 {
                a.accumulate(OBS, e(i), Fx::ONE);
            }
            a.select(OBS, &candidates, 100)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn observers_accumulate_independently() {
        let mut a = PriorityAccumulator::new();
        let other = e(2000);
        a.accumulate(OBS, e(1), Fx::from_int(5));
        assert_eq!(a.score(other, e(1)), Fx::ZERO);
        assert_eq!(a.tracked_pairs(), 1);
    }

    #[test]
    fn a_disconnected_observer_releases_its_entries() {
        // Entries are keyed by observer, so without this the map grows for the life of the process.
        let mut a = PriorityAccumulator::new();
        for i in 0..100u32 {
            a.accumulate(OBS, e(i), Fx::ONE);
            a.accumulate(e(999), e(i), Fx::ONE);
        }
        assert_eq!(a.tracked_pairs(), 200);
        a.remove_observer(OBS);
        assert_eq!(a.tracked_pairs(), 100);
    }

    #[test]
    fn forgetting_an_entity_clears_only_that_pair() {
        let mut a = PriorityAccumulator::new();
        a.accumulate(OBS, e(1), Fx::from_int(5));
        a.accumulate(OBS, e(2), Fx::from_int(5));
        let kept = a.score(OBS, e(2));
        a.forget(OBS, e(1));
        assert_eq!(a.score(OBS, e(1)), Fx::ZERO);
        assert_eq!(a.score(OBS, e(2)), kept, "the other pair is untouched");
        assert!(
            kept.raw() >= Fx::from_int(5).raw(),
            "and retains at least what it accumulated"
        );
    }

    #[test]
    fn priority_falls_off_with_distance_but_never_to_zero() {
        // A rule that reached zero would reintroduce permanent starvation through the back door.
        let rule = PriorityRule::default();
        let near = rule.gain(Fx::ZERO);
        let mid = rule.gain(Fx::from_int(50));
        let far = rule.gain(Fx::from_int(5000));

        assert_eq!(near, Fx::ONE);
        assert!(mid.raw() < near.raw() && mid.raw() > far.raw());
        assert!(
            far.raw() > 0,
            "even a very distant entity keeps accumulating"
        );
        // Halving distance should roughly halve the gain.
        assert!((mid.to_f64_lossy() - 0.5).abs() < 0.01, "{mid:?}");
    }

    #[test]
    fn an_oversized_entity_does_not_block_smaller_ones() {
        // A greedy fill must keep going past an item that does not fit, or one large entity at the
        // top of the ranking would waste the whole budget.
        let mut a = PriorityAccumulator::new();
        a.accumulate(OBS, e(1), Fx::from_int(10));
        a.accumulate(OBS, e(2), Fx::from_int(5));

        let before = a.score(OBS, e(1));
        let s = a.select(OBS, &[(e(1), 500), (e(2), 10)], 100);
        assert_eq!(s.entities, vec![e(2)]);
        assert_eq!(s.deferred, 1);
        assert_eq!(
            a.score(OBS, e(1)),
            before,
            "the entity that did not fit keeps its priority"
        );
    }
}
