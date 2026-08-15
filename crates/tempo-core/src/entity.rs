//! Entity identity and the simulation clock.

use core::fmt;

/// A handle to an entity: a slot index plus the generation occupying it.
///
/// The generation is what makes a stale handle detectable. Slot indices are reused when entities
/// despawn, so without it, a handle to a dead entity would silently address whatever took its
/// place — which across a network becomes a client applying updates meant for a different entity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    /// A handle that never refers to a live entity.
    pub const NONE: Entity = Entity {
        index: u32::MAX,
        generation: 0,
    };

    /// Constructs a handle from its parts. Prefer spawning through the world.
    #[inline]
    pub const fn from_parts(index: u32, generation: u32) -> Entity {
        Entity { index, generation }
    }

    /// The slot index, which is what the wire format transmits.
    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation occupying the slot when this handle was made.
    #[inline]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Entity::NONE {
            write!(f, "Entity::NONE")
        } else {
            write!(f, "Entity({}v{})", self.index, self.generation)
        }
    }
}

/// Allocates and recycles entity slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityAllocator {
    /// Current generation per slot. Incremented on despawn.
    generations: Vec<u32>,
    /// Whether each slot currently holds a live entity.
    alive: Vec<bool>,
    /// Slots available for reuse, most recently freed first.
    free: Vec<u32>,
    /// Number of slots currently marked alive.
    ///
    /// Tracked rather than derived. Deriving it as `slots - free.len()` assumes every slot not on
    /// the free list is alive, and `alloc_at` breaks that: claiming slot 150 in an empty allocator
    /// creates 150 slots that are neither alive nor free.
    live: usize,
}

impl EntityAllocator {
    /// Creates an empty allocator.
    pub fn new() -> EntityAllocator {
        EntityAllocator::default()
    }

    /// Number of slots ever allocated, live or not.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.generations.len()
    }

    /// Number of live entities.
    #[inline]
    pub fn live_count(&self) -> usize {
        self.live
    }

    /// Allocates an entity, reusing a free slot when one is available.
    pub fn alloc(&mut self) -> Entity {
        match self.free.pop() {
            Some(index) => {
                self.alive[index as usize] = true;
                self.live += 1;
                Entity {
                    index,
                    generation: self.generations[index as usize],
                }
            }
            None => {
                let index = self.generations.len() as u32;
                self.generations.push(0);
                self.alive.push(true);
                self.live += 1;
                Entity {
                    index,
                    generation: 0,
                }
            }
        }
    }

    /// Allocates a specific slot and generation, growing storage as needed.
    ///
    /// Used when applying a remote spawn: the authority chose the identity, and a client that
    /// allocated its own would diverge immediately.
    pub fn alloc_at(&mut self, entity: Entity) {
        let index = entity.index as usize;
        if index >= self.generations.len() {
            self.generations.resize(index + 1, 0);
            self.alive.resize(index + 1, false);
        }
        self.generations[index] = entity.generation;
        if !self.alive[index] {
            self.live += 1;
        }
        self.alive[index] = true;
        self.free.retain(|&f| f != entity.index);
    }

    /// True if the handle refers to a live entity of the matching generation.
    #[inline]
    pub fn is_alive(&self, e: Entity) -> bool {
        let i = e.index as usize;
        i < self.generations.len() && self.alive[i] && self.generations[i] == e.generation
    }

    /// Frees an entity's slot. Returns false if the handle was already stale.
    pub fn free(&mut self, e: Entity) -> bool {
        if !self.is_alive(e) {
            return false;
        }
        let i = e.index as usize;
        self.alive[i] = false;
        self.live -= 1;
        // Wrapping is correct here: after 2^32 reuses of one slot a handle could alias, which is
        // far beyond any realistic session and cheaper than tracking it.
        self.generations[i] = self.generations[i].wrapping_add(1);
        self.free.push(e.index);
        true
    }

    /// The live entity occupying `index`, if any.
    #[inline]
    pub fn entity_at(&self, index: u32) -> Option<Entity> {
        let i = index as usize;
        if i < self.generations.len() && self.alive[i] {
            Some(Entity {
                index,
                generation: self.generations[i],
            })
        } else {
            None
        }
    }

    /// Iterates live entities in ascending slot order.
    ///
    /// Ascending order is required, not incidental: the wire format encodes entity identity as
    /// gaps between consecutive indices, and the state hash is computed in this order.
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        (0..self.generations.len() as u32).filter_map(|i| self.entity_at(i))
    }

    /// Removes every entity.
    pub fn clear(&mut self) {
        self.generations.clear();
        self.alive.clear();
        self.free.clear();
        self.live = 0;
    }

    /// Raw generation and liveness arrays, for snapshotting.
    pub(crate) fn raw(&self) -> (&[u32], &[bool], &[u32]) {
        (&self.generations, &self.alive, &self.free)
    }

    /// Restores from raw arrays produced by [`EntityAllocator::raw`].
    pub(crate) fn restore(&mut self, generations: &[u32], alive: &[bool], free: &[u32]) {
        self.generations.clear();
        self.generations.extend_from_slice(generations);
        self.alive.clear();
        self.alive.extend_from_slice(alive);
        self.free.clear();
        self.free.extend_from_slice(free);
        self.live = alive.iter().filter(|&&a| a).count();
    }
}

/// A simulation tick number.
///
/// Ticks wrap. At 60 Hz a `u32` lasts about 2.3 years of continuous simulation, so wrapping is
/// theoretical — but comparisons still go through [`Tick::is_newer_than`] rather than `<`, because
/// the same comparison logic is used for packet sequence numbers, which wrap in hours.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Tick(pub u32);

impl Tick {
    /// The zero tick.
    pub const ZERO: Tick = Tick(0);
    /// Sentinel meaning "no baseline"; see `docs/spec/wire-protocol.md` §4.
    pub const NONE: Tick = Tick(u32::MAX);

    /// The next tick.
    #[inline]
    pub const fn next(self) -> Tick {
        Tick(self.0.wrapping_add(1))
    }

    /// This tick advanced by `n`.
    #[inline]
    pub const fn plus(self, n: u32) -> Tick {
        Tick(self.0.wrapping_add(n))
    }

    /// This tick moved back by `n`.
    #[inline]
    pub const fn minus(self, n: u32) -> Tick {
        Tick(self.0.wrapping_sub(n))
    }

    /// Signed distance from `other` to `self`, correct across wraparound.
    #[inline]
    pub const fn diff(self, other: Tick) -> i32 {
        self.0.wrapping_sub(other.0) as i32
    }

    /// True if `self` is more recent than `other`, correct across wraparound.
    #[inline]
    pub const fn is_newer_than(self, other: Tick) -> bool {
        self.diff(other) > 0
    }
}

impl fmt::Debug for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Tick::NONE {
            write!(f, "Tick::NONE")
        } else {
            write!(f, "Tick({})", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_reused_with_a_bumped_generation() {
        let mut a = EntityAllocator::new();
        let e0 = a.alloc();
        assert!(a.free(e0));

        let e1 = a.alloc();
        assert_eq!(e1.index(), e0.index(), "the slot is reused");
        assert_ne!(
            e1.generation(),
            e0.generation(),
            "but the generation moves on"
        );

        // This is the property the generation exists for: a stale handle must not address the
        // entity that took its place.
        assert!(!a.is_alive(e0));
        assert!(a.is_alive(e1));
    }

    #[test]
    fn freeing_a_stale_handle_is_a_no_op() {
        let mut a = EntityAllocator::new();
        let e = a.alloc();
        assert!(a.free(e));
        assert!(!a.free(e), "double free must not corrupt the free list");
        assert_eq!(a.live_count(), 0);
    }

    #[test]
    fn alloc_at_reproduces_a_remote_identity() {
        let mut a = EntityAllocator::new();
        let remote = Entity::from_parts(7, 3);
        a.alloc_at(remote);
        assert!(a.is_alive(remote));
        assert_eq!(a.entity_at(7), Some(remote));
        // Allocating locally must not hand out the slot the authority claimed.
        for _ in 0..10 {
            assert_ne!(a.alloc().index(), 7);
        }
    }

    #[test]
    fn alloc_at_removes_the_slot_from_the_free_list() {
        let mut a = EntityAllocator::new();
        let e = a.alloc();
        a.free(e);
        a.alloc_at(Entity::from_parts(e.index(), 9));
        assert_ne!(
            a.alloc().index(),
            e.index(),
            "the claimed slot must not be handed out again"
        );
    }

    #[test]
    fn live_count_is_correct_after_a_sparse_claim() {
        // alloc_at can create slots that are neither alive nor on the free list, so a live count
        // derived as `slots - free.len()` is wrong. Applying a remote spawn at a high index is the
        // ordinary case that hits this, not a corner case.
        let mut a = EntityAllocator::new();
        a.alloc_at(Entity::from_parts(150, 0));
        assert_eq!(a.live_count(), 1);
        assert_eq!(a.slot_count(), 151);
        assert_eq!(a.iter().count(), 1);

        a.alloc_at(Entity::from_parts(150, 1));
        assert_eq!(
            a.live_count(),
            1,
            "re-claiming an occupied slot must not double-count"
        );
    }

    #[test]
    fn live_count_survives_a_restore() {
        let mut a = EntityAllocator::new();
        a.alloc_at(Entity::from_parts(9, 0));
        let (g, al, f) = a.raw();
        let (g, al, f) = (g.to_vec(), al.to_vec(), f.to_vec());
        let mut b = EntityAllocator::new();
        b.restore(&g, &al, &f);
        assert_eq!(b.live_count(), 1);
    }

    #[test]
    fn iteration_is_ascending_and_skips_dead_slots() {
        let mut a = EntityAllocator::new();
        let e: Vec<Entity> = (0..5).map(|_| a.alloc()).collect();
        a.free(e[1]);
        a.free(e[3]);
        let live: Vec<u32> = a.iter().map(|x| x.index()).collect();
        assert_eq!(live, vec![0, 2, 4]);
    }

    #[test]
    fn tick_comparison_survives_wraparound() {
        // The reason comparisons do not use `<`. Near the wrap point a naive comparison inverts,
        // and the same logic serves packet sequence numbers, which wrap in hours rather than years.
        let late = Tick(u32::MAX - 1);
        let early = late.plus(3); // wrapped past zero
        assert!(early.is_newer_than(late));
        assert!(!late.is_newer_than(early));
        assert_eq!(early.diff(late), 3);
        assert_eq!(late.diff(early), -3);
    }

    #[test]
    fn tick_arithmetic_wraps() {
        assert_eq!(Tick(0).minus(1), Tick(u32::MAX));
        assert_eq!(Tick(u32::MAX).next(), Tick(0));
    }
}
