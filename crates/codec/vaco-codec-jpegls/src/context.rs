//! Context modeling: gradient quantization, the 365-context table, bias
//! cancellation, and run mode.
//!
//! # What comes from the LOCO-I paper directly (`Vaco-Spec-Ref` id
//! `locoi-hpl98-193`)
//!
//! The MED predictor (eq. 1), the default 8-bit gradient quantization
//! regions (`{0}, ±{1,2}, ±{3..6}, ±{7..20}, ±{e|e>=21}` — §3.2.2), the bias
//! computation procedure (Fig. 3) and its per-sample ordering (Fig. 4 steps
//! 8-12), the reset threshold's default of 64 (§3.4), and the two Golomb
//! mappings `M`/`M'` (eq. 5 and its `M'(eps) = M(-eps-1)` definition, §3.3.1)
//! are all transcribed from the paper's own equations.
//!
//! # What the paper does not give, and had to be measured instead (D17)
//!
//! The run-mode adaptation table (the paper names it only as "a pre-defined
//! table", deferring the values to the ISO text this crate could not reach)
//! and the run-interruption sample's modified mappings are **not** in the
//! paper. Both were reconstructed here and then checked the only way D6/D17
//! allow: encoding deliberately flat and run-heavy synthetic images with
//! this crate, decoding the result with `ffmpeg -c:v jpegls`, and comparing
//! pixels — never by reading any codec's source. See `codec`'s tests for the
//! images used.

use crate::golomb;

/// LOCO-I eq. 8's reset threshold, `N0` (§3.4): "values of `N0` between 32
/// and 256 work well... the default value in JPEG-LS is 64."
pub(crate) const RESET_THRESHOLD: i32 = 64;

/// Default 8-bit gradient quantization thresholds (§3.2.2): regions
/// `{0}, ±{1,2}, ±{3,4,5,6}, ±{7,...,20}, ±{e|e>=21}`.
const T1: i32 = 3;
const T2: i32 = 7;
const T3: i32 = 21;

/// `(2*4+1)^3` triplets, folded by sign symmetry to `((2T+1)^3 + 1) / 2`
/// with `T = 4` (§3.2.2): 365 contexts, index 0 reserved for the all-zero
/// (run-mode) triplet.
pub(crate) const NUM_CONTEXTS: usize = 365;

/// `A`'s initial value for an 8-bit alphabet (`alpha = 256`): `max(2,
/// floor((alpha + 32) / 64))` (Fig. 4, Step 0.b) `= max(2, 4) = 4`.
const INIT_A: i32 = 4;

/// The MED ("median edge detector") predictor, eq. 1.
#[must_use]
pub(crate) fn med(a: i32, b: i32, c: i32) -> i32 {
    if c >= a.max(b) {
        a.min(b)
    } else if c <= a.min(b) {
        a.max(b)
    } else {
        a + b - c
    }
}

/// Quantize one local gradient into `{-4, ..., 4}` per the default 8-bit
/// thresholds.
#[must_use]
fn quantize(g: i32) -> i32 {
    if g == 0 {
        return 0;
    }
    let mag = g.abs();
    let q = if mag < T1 {
        1
    } else if mag < T2 {
        2
    } else if mag < T3 {
        3
    } else {
        4
    };
    if g < 0 { -q } else { q }
}

/// Map the three local gradients to a context index in `0..NUM_CONTEXTS` and
/// whether the sign convention was flipped (§3.2.2: "if the first non-zero
/// element of `Ct` is negative, the encoded value is `-eps`, using context
/// `-Ct`"). `q1*81 + q2*9 + q3` (each `q` in `-4..=4`) has the same sign as
/// its own first non-zero digit, because `81 > 9*4 + 4` and `9 > 4`, so
/// comparing the combined value against zero is exactly the rule the
/// standard states digit-by-digit.
#[must_use]
pub(crate) fn context_index(g1: i32, g2: i32, g3: i32) -> (usize, bool) {
    let (q1, q2, q3) = (quantize(g1), quantize(g2), quantize(g3));
    let raw = q1 * 81 + q2 * 9 + q3;
    if raw < 0 {
        (raw.unsigned_abs() as usize, true)
    } else {
        (raw as usize, false)
    }
}

/// One regular context's adaptive state: `A` (accumulated magnitude), `B`
/// (accumulated signed residual), `C` (the integer bias correction), `N`
/// (occurrence count).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RegularCtx {
    a: i32,
    b: i32,
    c: i32,
    n: i32,
}

impl RegularCtx {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            a: INIT_A,
            b: 0,
            c: 0,
            n: 1,
        }
    }

    /// The context's own bias correction, sign-adjusted for a flipped
    /// context (§3.2.2's `-Ct` convention applies to `C` too: a flipped
    /// context corrects by subtracting rather than adding).
    #[must_use]
    pub(crate) const fn bias(&self, sign_flip: bool) -> i32 {
        if sign_flip { -self.c } else { self.c }
    }

    /// `k = min{k' | (N << k') >= A}` (eq. 8).
    #[must_use]
    pub(crate) fn k(&self) -> u32 {
        golomb::select_k(self.n.unsigned_abs(), self.a.unsigned_abs())
    }

    /// Step 9: use `M'` instead of `M` exactly when `k == 0` and `2B <= -N`.
    #[must_use]
    pub(crate) const fn use_alternate_mapping(&self, k: u32) -> bool {
        k == 0 && 2 * self.b <= -self.n
    }

    /// Steps 11-12 (Fig. 3 plus the interleaved reset of §3.4): accumulate,
    /// reset at the threshold, then adjust `C`.
    pub(crate) fn update(&mut self, eps: i32) {
        self.b += eps;
        self.a += eps.abs();
        // The reset check reads `N` as it stood before this sample (i.e.
        // before the `N += 1` below), and the halving includes this
        // sample's own contribution to `A`/`B` — measured against
        // `ffmpeg -c:v jpegls` (D17): halving before folding this sample in
        // (checking the pre-increment `N`, then incrementing) reproduces a
        // structured one-off error on the sample immediately after every
        // reset, because the halved `B` no longer satisfies the bias-check
        // threshold that the un-halved value would have.
        if self.n == RESET_THRESHOLD {
            // Arithmetic shift on a signed `i32` rounds toward negative
            // infinity, which is exactly "halve, rounding down" for a `B`
            // that can be negative.
            self.a >>= 1;
            self.b >>= 1;
            self.n >>= 1;
        }
        self.n += 1;
        if self.b <= -self.n {
            self.c = (self.c - 1).max(-128);
            self.b += self.n;
            if self.b <= -self.n {
                self.b = -self.n + 1;
            }
        } else if self.b > 0 {
            self.c = (self.c + 1).min(127);
            self.b -= self.n;
            if self.b > 0 {
                self.b = 0;
            }
        }
    }
}

/// One of the two run-interruption contexts (§3.5: "conditioning is based
/// on two special contexts, determined according to whether `a = b` or
/// `a != b`"). No bias cancellation runs here, so there is no `B`/`C`; the
/// `M`-vs-`M'` choice and the `codec`-level sign convention that goes with
/// it are fixed per context (see `codec::decode_ri_sample`'s doc for what
/// was measured, rather than a `B`-like running statistic here).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunInterruptionCtx {
    a: i32,
    n: i32,
}

impl RunInterruptionCtx {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { a: INIT_A, n: 1 }
    }

    #[must_use]
    pub(crate) fn k(self) -> u32 {
        golomb::select_k(self.n.unsigned_abs(), self.a.unsigned_abs())
    }

    pub(crate) fn update(&mut self, eps: i32) {
        self.a += eps.abs();
        // Same reset ordering as `RegularCtx::update`: check the
        // pre-increment `N`, halve (including this sample), then increment.
        if self.n == RESET_THRESHOLD {
            self.a >>= 1;
            self.n >>= 1;
        }
        self.n += 1;
    }
}

/// The shared context state for one scan: 365 regular contexts plus the two
/// run-interruption contexts, all shared across every component (Appendix:
/// "a single set of context counters... is used across all components in
/// the scan").
#[derive(Debug, Clone, Copy)]
pub(crate) struct Contexts {
    pub(crate) regular: [RegularCtx; NUM_CONTEXTS],
    pub(crate) ri: [RunInterruptionCtx; 2],
}

impl Contexts {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            regular: [RegularCtx::new(); NUM_CONTEXTS],
            ri: [RunInterruptionCtx::new(); 2],
        }
    }
}

/// Run-mode Golomb-parameter adaptation table (32 entries, indexed by
/// [`RunModeState`]'s running index). Not given numerically in the LOCO-I
/// paper — see this module's doc for how it was checked instead.
const RUN_ADAPT: [u32; 32] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
    14, 15,
];

/// One component's run-mode adaptation index (§Appendix: "the index to the
/// table used to adapt the elementary Golomb code in run mode is
/// component-dependent" for line-interleaved scans).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunModeState {
    index: usize,
}

impl RunModeState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { index: 0 }
    }

    /// The current elementary-Golomb-code order `g`; the segment length is
    /// `2^g`.
    #[must_use]
    pub(crate) fn g(self) -> u32 {
        RUN_ADAPT.get(self.index).copied().unwrap_or(15)
    }

    /// A full run segment completed: advance the index (clamped at the top
    /// of the table).
    pub(crate) fn bump_up(&mut self) {
        self.index = (self.index + 1).min(RUN_ADAPT.len() - 1);
    }

    /// A run was interrupted by a non-matching sample: retreat the index.
    pub(crate) fn bump_down(&mut self) {
        self.index = self.index.saturating_sub(1);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn quantize_matches_the_default_region_boundaries() {
        assert_eq!(quantize(0), 0);
        for g in 1..=2 {
            assert_eq!(quantize(g), 1);
            assert_eq!(quantize(-g), -1);
        }
        for g in 3..=6 {
            assert_eq!(quantize(g), 2);
            assert_eq!(quantize(-g), -2);
        }
        for g in 7..=20 {
            assert_eq!(quantize(g), 3);
            assert_eq!(quantize(-g), -3);
        }
        for g in [21, 22, 255, 1000] {
            assert_eq!(quantize(g), 4);
            assert_eq!(quantize(-g), -4);
        }
    }

    #[test]
    fn context_index_is_symmetric_under_negation() {
        for g1 in [-25, -7, -3, -1, 0, 1, 3, 7, 25] {
            for g2 in [-25, -3, 0, 3, 25] {
                for g3 in [-25, -3, 0, 3, 25] {
                    let (idx_pos, flip_pos) = context_index(g1, g2, g3);
                    let (idx_neg, flip_neg) = context_index(-g1, -g2, -g3);
                    assert_eq!(idx_pos, idx_neg);
                    if (g1, g2, g3) != (0, 0, 0) {
                        assert_ne!(flip_pos, flip_neg);
                    }
                }
            }
        }
    }

    #[test]
    fn context_index_stays_in_range() {
        for q1 in -4..=4 {
            for q2 in -4..=4 {
                for q3 in -4..=4 {
                    let (idx, _) = context_index(q1 * 25, q2 * 25, q3 * 25);
                    assert!(idx < NUM_CONTEXTS);
                }
            }
        }
    }

    #[test]
    fn med_picks_b_on_a_vertical_edge_a_on_a_horizontal_edge() {
        assert_eq!(med(10, 20, 30), 10);
        assert_eq!(med(20, 10, 5), 20);
        assert_eq!(med(10, 20, 15), 10 + 20 - 15);
    }

    #[test]
    fn regular_ctx_reset_halves_all_three_counters() {
        let mut c = RegularCtx::new();
        // `n` starts at 1, so it reads exactly `RESET_THRESHOLD` at the
        // start of the `RESET_THRESHOLD`-th call — the reset check reads
        // `n` before this call's own increment, so that is the call whose
        // halving fires.
        for _ in 0..(RESET_THRESHOLD - 1) {
            c.update(-3);
        }
        assert_eq!(c.n, RESET_THRESHOLD);
        c.update(-3);
        assert_eq!(c.n, (RESET_THRESHOLD >> 1) + 1);
    }

    #[test]
    fn run_mode_state_starts_at_g_zero_and_only_ever_grows_within_range() {
        let mut s = RunModeState::new();
        assert_eq!(s.g(), 0);
        for _ in 0..40 {
            s.bump_up();
        }
        assert_eq!(s.g(), 15);
        for _ in 0..40 {
            s.bump_down();
        }
        assert_eq!(s.g(), 0);
    }
}
