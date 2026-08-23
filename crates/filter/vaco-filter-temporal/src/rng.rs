//! A small, dependency-free, deterministic PRNG for [`crate::random`].
//!
//! This is **not** an attempt to reproduce the reference's own bit stream —
//! D7/D17 mean that would require reading its source, and the workspace has
//! no `rand`-family crate pulled in yet for a single filter to justify. What
//! `random` needs from its `seed` option is *reproducibility* (the same seed
//! always shuffles the same way) and *a real shuffle* (uniform-ish over the
//! cache window), not bit-identical output — see `docs/filter/
//! vaco-filter-temporal.md` for the documented divergence.
//!
//! `SplitMix64` (Vigna, public domain / CC0, the generator behind Java's
//! `SplittableRandom`) is used because its output-mixing step is a few
//! published lines, has no periodicity concerns at the lengths this filter
//! ever runs for, and needs no crate.
#![allow(
    clippy::unreadable_literal,
    reason = "the SplitMix64 constants are the published magic numbers"
)]

#[derive(Debug, Clone, Copy)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64-bit output, advancing the state.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A uniform index in `0..bound`, via Lemire's rejection-free-in-practice
    /// widening reduction (slight modulo bias only at the very top of the
    /// range, irrelevant at the cache sizes this filter uses).
    pub(crate) fn next_below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let r = self.next_u64();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the u128 product's high 64 bits are < bound <= usize::MAX"
        )]
        let scaled = ((u128::from(r) * u128::from(bound as u64)) >> 64) as u64;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scaled < bound, and bound is a usize"
        )]
        {
            scaled as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_the_same_sequence() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let seq_a: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let seq_b: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        assert_eq!(seq_a, seq_b);
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn next_below_stays_in_range() {
        let mut r = SplitMix64::new(7);
        for _ in 0..1000 {
            let v = r.next_below(30);
            assert!(v < 30);
        }
    }

    #[test]
    fn next_below_zero_is_zero() {
        let mut r = SplitMix64::new(7);
        assert_eq!(r.next_below(0), 0);
    }
}
