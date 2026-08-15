//! A small deterministic pseudo-random generator.
//!
//! Used only by the simulated link, and deliberately not a dependency on `rand`: the whole point of
//! the simulated link is that a seed reproduces a network condition exactly, on every platform and
//! in every language binding. That requires an algorithm specified here rather than one whose
//! implementation may change between crate versions.
//!
//! Probabilities are expressed in **parts per million as integers**, never as floats. A float
//! probability would reintroduce exactly the platform variation the rest of the project works to
//! eliminate ([ADR-0002](../../../docs/adr/0002-fixed-point-determinism.md)).

/// xorshift64*, seeded and reproducible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator from a seed.
    ///
    /// A zero seed would make xorshift produce zeros forever, so it is replaced with a fixed
    /// non-zero constant rather than rejected — a test writer passing `0` should get a working
    /// generator, not a panic.
    pub const fn new(seed: u64) -> Rng {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Produces the next value in the sequence.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n`. Returns 0 when `n` is 0.
    ///
    /// Uses the widening-multiply method, which is unbiased enough for link simulation and avoids
    /// the rejection loop that would make the number of generator calls data-dependent — and
    /// therefore make one run's sequence diverge from another's.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }

    /// A value in `lo..=hi`. Returns `lo` if `hi < lo`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        lo + self.below(hi - lo + 1)
    }

    /// True with probability `ppm` parts per million.
    ///
    /// `chance(0)` is never true and `chance(1_000_000)` is always true, both exactly — a link
    /// configured with zero loss must drop nothing at all, not almost nothing.
    pub fn chance(&mut self, ppm: u32) -> bool {
        if ppm == 0 {
            return false;
        }
        if ppm >= 1_000_000 {
            return true;
        }
        self.below(1_000_000) < ppm as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_reproduces_the_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn a_zero_seed_still_generates() {
        // xorshift on zero yields zeros forever. A test writer passing 0 should get a working
        // generator rather than a silently dead one.
        let mut r = Rng::new(0);
        let first = r.next_u64();
        assert_ne!(first, 0);
        assert_ne!(r.next_u64(), first);
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = Rng::new(7);
        for n in [1u64, 2, 10, 1000, u64::MAX] {
            for _ in 0..200 {
                assert!(r.below(n) < n, "below({n}) escaped its bound");
            }
        }
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn range_is_inclusive_and_handles_inversion() {
        let mut r = Rng::new(9);
        for _ in 0..500 {
            let v = r.range(10, 20);
            assert!((10..=20).contains(&v), "range escaped: {v}");
        }
        assert_eq!(r.range(5, 5), 5);
        assert_eq!(r.range(9, 3), 9, "an inverted range yields the low bound");
    }

    #[test]
    fn chance_endpoints_are_exact() {
        // A link configured with zero loss must drop nothing at all, not almost nothing.
        let mut r = Rng::new(3);
        for _ in 0..10_000 {
            assert!(!r.chance(0));
            assert!(r.chance(1_000_000));
        }
    }

    #[test]
    fn chance_is_roughly_calibrated() {
        let mut r = Rng::new(11);
        let hits = (0..100_000).filter(|_| r.chance(100_000)).count();
        // 10% of 100k, generous tolerance — this checks calibration, not distribution quality.
        assert!(
            (9_000..11_000).contains(&hits),
            "expected ~10000 hits, got {hits}"
        );
    }
}
