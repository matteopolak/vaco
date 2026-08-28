//! A small, dependency-free, deterministic PRNG for [`crate::noise`].
//!
//! Not an attempt to reproduce the reference's own bit stream (D7/D17 —
//! that would need its source, and the workspace has no `rand`-family crate
//! pulled in for one filter to justify adding). What `noise` needs from its
//! `seed` option is *reproducibility*, not bit-identical output. This is a
//! third copy of the same `SplitMix64` (Vigna, public domain/CC0) already
//! duplicated in `vaco-filter-temporal::rng` and `vaco-filter-source::rng`
//! for the identical reason; see `planning/TECH-DEBT.md` for the
//! consolidation note this pass recorded rather than acted on, since the
//! obvious host (`vaco-filter-vdsp`) had a live owner at the time.
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

    /// A signed value in `-amount..=amount`, for a per-pixel noise excursion.
    pub(crate) fn next_signed(&mut self, amount: i32) -> i32 {
        if amount <= 0 {
            return 0;
        }
        let span = u64::from(amount.unsigned_abs()) * 2 + 1;
        let r = self.next_u64();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the u128 product's high 64 bits are < span <= u32::MAX * 2 + 1"
        )]
        let scaled = ((u128::from(r) * u128::from(span)) >> 64) as u64;
        #[allow(
            clippy::cast_possible_wrap,
            clippy::cast_possible_truncation,
            reason = "scaled < span = 2*amount+1 <= i32::MAX range"
        )]
        {
            scaled as i32 - amount
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
    fn next_signed_stays_in_bounds() {
        let mut r = SplitMix64::new(7);
        for _ in 0..2000 {
            let v = r.next_signed(20);
            assert!((-20..=20).contains(&v));
        }
    }

    #[test]
    fn next_signed_zero_amount_is_zero() {
        let mut r = SplitMix64::new(7);
        assert_eq!(r.next_signed(0), 0);
    }
}
