//! A small, dependency-free, deterministic PRNG for the `seed`/`random_seed`
//! options on [`crate::gradients`], [`crate::cellauto`], [`crate::life`],
//! [`crate::sierpinski`] (triangle mode) and [`crate::perlin`] (`random`/`seed`
//! modes).
//!
//! **Not** an attempt to reproduce the reference's own bit stream (`av_lfg`,
//! a lagged Fibonacci generator) — D7/D17 mean that would require reading its
//! source, and reverse-engineering an LFG's internal state purely from output
//! samples is not a tractable black-box measurement in the time this crate
//! had. What every one of the filters above needs from its seed option is
//! *reproducibility* (the same seed always produces the same frame) and *a
//! real shuffle*, not bit-identical output. `vaco-filter-temporal`'s `random`
//! filter carries the identical divergence for the identical reason — see
//! that crate's `rng.rs` doc — and this is the same `SplitMix64` (Vigna,
//! public domain / CC0) for the same reasons: a few published lines, no
//! periodicity concerns at these sizes, no crate to pull in.
//!
//! Every seeded generator in this crate documents this divergence in
//! `docs/filter/vaco-filter-source.md` rather than claiming exactness it does
//! not have.
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

    /// A uniform-ish `f64` in `[0, 1)`.
    pub(crate) fn next_f64(&mut self) -> f64 {
        // 53 bits of mantissa precision, the same construction `rand` uses.
        let bits = self.next_u64() >> 11;
        #[allow(
            clippy::cast_precision_loss,
            reason = "53 significant bits fit exactly in an f64 mantissa"
        )]
        {
            (bits as f64) * (1.0 / ((1u64 << 53) as f64))
        }
    }

    /// A uniform index in `0..bound`, via Lemire's rejection-free-in-practice
    /// widening reduction.
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

    /// A byte in `0..=255`, for RGB channel randomisation.
    pub(crate) fn next_byte(&mut self) -> u8 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "truncating to the low byte is the intended use"
        )]
        {
            self.next_u64() as u8
        }
    }
}

/// Turns the `seed = -1` "pick one for me" convention (shared by every
/// generator in this crate) into a real seed: negative means "derive one from
/// the option's declared default", which we make deterministic (unlike the
/// reference, which reads from `/dev/urandom` or the clock) so a graph built
/// twice from the same command line behaves the same way. Programs that want
/// true randomness pass an explicit `seed`.
pub(crate) const fn resolve_seed(seed: i64, fallback: u64) -> u64 {
    if seed < 0 {
        fallback
    } else {
        #[allow(clippy::cast_sign_loss, reason = "seed >= 0 was just checked")]
        {
            seed as u64
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
            assert!(r.next_below(30) < 30);
        }
    }

    #[test]
    fn next_f64_stays_in_unit_interval() {
        let mut r = SplitMix64::new(9);
        for _ in 0..1000 {
            let v = r.next_f64();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn resolve_seed_negative_uses_fallback() {
        assert_eq!(resolve_seed(-1, 99), 99);
        assert_eq!(resolve_seed(5, 99), 5);
    }
}
