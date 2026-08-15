//! Deciding which entities are relevant to which observer.
//!
//! Sending every entity to every client is O(entities × clients). At 10,000 entities and 500
//! clients that is five million entity-updates per snapshot — impossible at any tick rate, and
//! pointless, because a client can only see a small fraction of the world.
//!
//! Filtering by visibility is the first half of the answer. The second half is
//! [`crate::priority`], because filtering alone still leaves more relevant entities than the
//! bandwidth budget can carry in a crowded area.

use std::collections::{HashMap, HashSet};

use tempo_core::Entity;
use tempo_fixed::{Fx, Vec2};

/// How relevance is decided.
pub enum InterestStrategy {
    /// Everything is relevant to everyone.
    ///
    /// The correct default. Below roughly fifty entities, filtering costs more than it saves, and
    /// anything cleverer is premature.
    Everything,
    /// Relevant within a radius, accelerated by a uniform grid.
    GridAoi {
        /// Width of a grid cell.
        cell_size: Fx,
        /// Relevance radius.
        radius: Fx,
        /// Extra distance an entity must travel *beyond* the radius before it stops being
        /// relevant.
        ///
        /// Without hysteresis an entity sitting exactly on the boundary flips in and out every
        /// tick, and each transition costs a full baseline — the most expensive thing the
        /// replication layer does.
        hysteresis: Fx,
    },
    /// An application-supplied predicate.
    ///
    /// The escape hatch for team visibility, room membership, line of sight, or any rule the
    /// engine has no business knowing about.
    Custom(Box<dyn Fn(Entity, Vec2, Entity, Vec2) -> bool + Send + Sync>),
}

impl core::fmt::Debug for InterestStrategy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InterestStrategy::Everything => write!(f, "Everything"),
            InterestStrategy::GridAoi {
                cell_size,
                radius,
                hysteresis,
            } => f
                .debug_struct("GridAoi")
                .field("cell_size", cell_size)
                .field("radius", radius)
                .field("hysteresis", hysteresis)
                .finish(),
            InterestStrategy::Custom(_) => write!(f, "Custom(..)"),
        }
    }
}

impl InterestStrategy {
    /// A grid strategy with hysteresis set to a tenth of the radius.
    pub fn grid(cell_size: Fx, radius: Fx) -> InterestStrategy {
        InterestStrategy::GridAoi {
            cell_size,
            radius,
            hysteresis: radius.div(Fx::from_int(10)),
        }
    }
}

/// A change in what an observer can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelevanceChange {
    /// The entity became relevant and needs a full baseline.
    Entered(Entity),
    /// The entity stopped being relevant and the observer should forget it.
    Left(Entity),
}

/// A uniform spatial grid.
///
/// Insert and query are O(1) in the number of entities, which is what makes per-client relevance
/// affordable at high entity counts.
#[derive(Debug, Default)]
pub struct SpatialGrid {
    cells: HashMap<(i32, i32), Vec<Entity>>,
    positions: HashMap<Entity, Vec2>,
    cell_size: Fx,
}

impl SpatialGrid {
    /// Creates a grid with the given cell width.
    pub fn new(cell_size: Fx) -> SpatialGrid {
        assert!(cell_size.raw() > 0, "cell size must be positive");
        SpatialGrid {
            cells: HashMap::new(),
            positions: HashMap::new(),
            cell_size,
        }
    }

    fn cell_of(&self, p: Vec2) -> (i32, i32) {
        (
            p.x.div(self.cell_size).to_int_floor() as i32,
            p.y.div(self.cell_size).to_int_floor() as i32,
        )
    }

    /// Inserts or moves an entity.
    pub fn insert(&mut self, entity: Entity, position: Vec2) {
        if let Some(old) = self.positions.get(&entity).copied() {
            let old_cell = self.cell_of(old);
            let new_cell = self.cell_of(position);
            if old_cell == new_cell {
                self.positions.insert(entity, position);
                return;
            }
            if let Some(v) = self.cells.get_mut(&old_cell) {
                v.retain(|e| *e != entity);
                if v.is_empty() {
                    self.cells.remove(&old_cell);
                }
            }
        }
        self.cells
            .entry(self.cell_of(position))
            .or_default()
            .push(entity);
        self.positions.insert(entity, position);
    }

    /// Removes an entity.
    pub fn remove(&mut self, entity: Entity) {
        if let Some(p) = self.positions.remove(&entity) {
            let cell = self.cell_of(p);
            if let Some(v) = self.cells.get_mut(&cell) {
                v.retain(|e| *e != entity);
                if v.is_empty() {
                    self.cells.remove(&cell);
                }
            }
        }
    }

    /// An entity's recorded position.
    pub fn position(&self, entity: Entity) -> Option<Vec2> {
        self.positions.get(&entity).copied()
    }

    /// Number of entities tracked.
    #[inline]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// True if nothing is tracked.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Number of occupied cells. Sparse: empty regions cost nothing.
    #[inline]
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Entities within `radius` of `centre`.
    ///
    /// Returned in ascending entity order, not grid order. Iteration order of a hash map would
    /// otherwise leak into the replication order and make snapshots differ between runs for
    /// identical state.
    pub fn query(&self, centre: Vec2, radius: Fx) -> Vec<Entity> {
        let (cx, cy) = self.cell_of(centre);
        let reach = radius.div(self.cell_size).to_int_floor() as i32 + 1;
        let radius_sq = radius.mul(radius);

        let mut out = Vec::new();
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                let Some(bucket) = self.cells.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for &e in bucket {
                    let Some(p) = self.positions.get(&e) else {
                        continue;
                    };
                    // Squared distance: avoids a square root per candidate, and the comparison is
                    // exact either way.
                    if p.sub(centre).length_sq().raw() <= radius_sq.raw() {
                        out.push(e);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.positions.clear();
    }
}

/// Tracks what each observer currently considers relevant, and reports the transitions.
#[derive(Debug, Default)]
pub struct RelevanceTracker {
    visible: HashMap<Entity, HashSet<Entity>>,
}

impl RelevanceTracker {
    /// Creates an empty tracker.
    pub fn new() -> RelevanceTracker {
        RelevanceTracker::default()
    }

    /// Replaces an observer's relevant set and reports what changed.
    ///
    /// Transitions are events, not silent changes: an entity entering relevance needs a full
    /// baseline, because the observer has nothing to delta against. Without that, a re-entering
    /// entity would be encoded against a baseline the observer no longer holds and would decode
    /// to garbage.
    pub fn update(&mut self, observer: Entity, now_relevant: &[Entity]) -> Vec<RelevanceChange> {
        let current: HashSet<Entity> = now_relevant.iter().copied().collect();
        let previous = self.visible.entry(observer).or_default();

        let mut changes: Vec<RelevanceChange> = Vec::new();
        for e in current.difference(previous) {
            changes.push(RelevanceChange::Entered(*e));
        }
        for e in previous.difference(&current) {
            changes.push(RelevanceChange::Left(*e));
        }

        // Sorted so the change list is reproducible; set iteration order is not.
        changes.sort_unstable_by_key(|c| match c {
            RelevanceChange::Entered(e) | RelevanceChange::Left(e) => (e.index(), e.generation()),
        });

        *previous = current;
        changes
    }

    /// What an observer currently sees, in ascending order.
    pub fn visible(&self, observer: Entity) -> Vec<Entity> {
        let mut out: Vec<Entity> = self
            .visible
            .get(&observer)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        out.sort_unstable();
        out
    }

    /// True if the observer currently considers the entity relevant.
    pub fn is_visible(&self, observer: Entity, entity: Entity) -> bool {
        self.visible
            .get(&observer)
            .is_some_and(|s| s.contains(&entity))
    }

    /// Forgets an observer, when a client disconnects.
    pub fn remove_observer(&mut self, observer: Entity) {
        self.visible.remove(&observer);
    }

    /// Number of observers tracked.
    #[inline]
    pub fn observer_count(&self) -> usize {
        self.visible.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: u32) -> Entity {
        Entity::from_parts(i, 0)
    }

    fn at(x: i32, y: i32) -> Vec2 {
        Vec2::from_ints(x, y)
    }

    #[test]
    fn the_grid_finds_neighbours_within_a_radius() {
        let mut g = SpatialGrid::new(Fx::from_int(10));
        g.insert(e(0), at(0, 0));
        g.insert(e(1), at(5, 0));
        g.insert(e(2), at(50, 0));

        let near = g.query(at(0, 0), Fx::from_int(10));
        assert_eq!(near, vec![e(0), e(1)]);
        assert!(!near.contains(&e(2)));
    }

    #[test]
    fn results_are_ordered_by_entity_not_by_grid_layout() {
        // Hash-map iteration order would otherwise leak into the replication order and make
        // snapshots differ between runs for identical state.
        let mut g = SpatialGrid::new(Fx::from_int(5));
        for i in (0..20u32).rev() {
            g.insert(e(i), at(i as i32, 0));
        }
        let found = g.query(at(10, 0), Fx::from_int(100));
        let mut sorted = found.clone();
        sorted.sort_unstable();
        assert_eq!(found, sorted);
    }

    #[test]
    fn moving_an_entity_updates_its_cell() {
        let mut g = SpatialGrid::new(Fx::from_int(10));
        g.insert(e(0), at(0, 0));
        assert_eq!(g.query(at(0, 0), Fx::from_int(5)), vec![e(0)]);

        g.insert(e(0), at(100, 100));
        assert!(g.query(at(0, 0), Fx::from_int(5)).is_empty());
        assert_eq!(g.query(at(100, 100), Fx::from_int(5)), vec![e(0)]);
        assert_eq!(g.len(), 1, "moving must not duplicate the entity");
    }

    #[test]
    fn empty_regions_cost_nothing() {
        // Sparse by construction: a world spanning millions of units must not allocate a cell per
        // square unit.
        let mut g = SpatialGrid::new(Fx::from_int(1));
        g.insert(e(0), at(0, 0));
        g.insert(e(1), at(1_000_000, 1_000_000));
        assert_eq!(g.cell_count(), 2);
    }

    #[test]
    fn removal_cleans_up_the_cell() {
        let mut g = SpatialGrid::new(Fx::from_int(10));
        g.insert(e(0), at(0, 0));
        g.remove(e(0));
        assert!(g.is_empty());
        assert_eq!(g.cell_count(), 0, "an emptied cell should not linger");
        assert_eq!(g.position(e(0)), None);
    }

    #[test]
    fn queries_span_cell_boundaries() {
        // The failure mode a naive single-cell lookup has: a neighbour just across a boundary.
        let mut g = SpatialGrid::new(Fx::from_int(10));
        g.insert(e(0), at(9, 9));
        g.insert(e(1), at(11, 11));
        let found = g.query(at(10, 10), Fx::from_int(5));
        assert_eq!(found, vec![e(0), e(1)]);
    }

    #[test]
    fn negative_coordinates_work() {
        // Integer division truncates toward zero while cell indexing must floor, so this is where
        // an off-by-one shows up.
        let mut g = SpatialGrid::new(Fx::from_int(10));
        g.insert(e(0), at(-5, -5));
        g.insert(e(1), at(-15, -15));
        assert_eq!(g.query(at(-5, -5), Fx::from_int(3)), vec![e(0)]);
        assert_eq!(g.query(at(-10, -10), Fx::from_int(10)), vec![e(0), e(1)]);
    }

    #[test]
    fn the_tracker_reports_entries_and_exits() {
        let mut t = RelevanceTracker::new();
        let observer = e(100);

        let changes = t.update(observer, &[e(1), e(2)]);
        assert_eq!(
            changes,
            vec![
                RelevanceChange::Entered(e(1)),
                RelevanceChange::Entered(e(2))
            ]
        );

        let changes = t.update(observer, &[e(2), e(3)]);
        assert_eq!(
            changes,
            vec![RelevanceChange::Left(e(1)), RelevanceChange::Entered(e(3))]
        );
        assert_eq!(t.visible(observer), vec![e(2), e(3)]);
    }

    #[test]
    fn an_unchanged_set_reports_nothing() {
        // Transitions cost a full baseline, so reporting a spurious one is expensive.
        let mut t = RelevanceTracker::new();
        let observer = e(100);
        t.update(observer, &[e(1), e(2)]);
        assert!(
            t.update(observer, &[e(2), e(1)]).is_empty(),
            "order must not matter"
        );
    }

    #[test]
    fn observers_are_independent() {
        let mut t = RelevanceTracker::new();
        t.update(e(100), &[e(1)]);
        t.update(e(200), &[e(2)]);
        assert!(t.is_visible(e(100), e(1)));
        assert!(!t.is_visible(e(100), e(2)));
        assert!(t.is_visible(e(200), e(2)));
        assert_eq!(t.observer_count(), 2);
    }

    #[test]
    fn a_disconnected_observer_is_forgotten() {
        let mut t = RelevanceTracker::new();
        t.update(e(100), &[e(1), e(2)]);
        t.remove_observer(e(100));
        assert_eq!(t.observer_count(), 0);
        // Re-connecting starts fresh, so everything enters again and gets a baseline.
        let changes = t.update(e(100), &[e(1)]);
        assert_eq!(changes, vec![RelevanceChange::Entered(e(1))]);
    }

    #[test]
    fn change_lists_are_reproducible() {
        // Set iteration order is not stable, so the change list must be sorted explicitly or two
        // identical states would produce differently ordered replication.
        let run = || {
            let mut t = RelevanceTracker::new();
            t.update(e(0), &(1..30).map(e).collect::<Vec<_>>());
            t.update(e(0), &(15..45).map(e).collect::<Vec<_>>())
        };
        assert_eq!(run(), run());
    }
}
