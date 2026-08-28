//! Sine window generation for MDCT-based codecs (D-06).
//!
//! # What this is
//!
//! The "sine window" is the simpler of AAC's two window shapes (ISO/IEC
//! 14496-3 subpart 4 §4.6.3, `window_shape == 0`):
//!
//! ```text
//! win[i] = sin( (π / N) * (i + 0.5) )      for i in 0..N
//! ```
//!
//! where `N` is the *full* window length — twice the transform block size:
//! 2048 for AAC's `ONLY_LONG`/`LONG_START`/`LONG_STOP` sequences' 1024-line
//! MDCT, 256 for each of `EIGHT_SHORT`'s eight 128-line MDCTs. The same
//! formula (with its own `N`) is Vorbis's and MP2.5-AAC's sine window, and
//! generically the window every plain (non-KBD) MDCT-based codec applies for
//! time-domain alias cancellation (TDAC) — hence a shared crate rather than
//! another per-codec copy.
//!
//! KBD (Kaiser-Bessel-Derived), AAC's other window shape, is **not** here:
//! it is a materially different, iterative construction (a running sum over
//! Bessel-function terms, not a closed-form `sin`), and D-06 names this crate
//! for the sine window specifically. `vaco-codec-aac`'s own window-shape
//! selection ships sine-only for the same reason its module doc gives; KBD
//! is a disclosed gap there, not silently approximated by this crate's
//! output.
//!
//! # The correctness property this crate is tested against
//!
//! Not "does this look like a sine curve" but the property TDAC actually
//! needs — the Princen-Bradley condition, ISO/IEC 14496-3 subpart 4 §4.6.3's
//! own requirement on any window it accepts:
//!
//! ```text
//! win[i]^2 + win[i + N/2]^2 == 1     for every i in 0..N/2
//! ```
//!
//! A window failing this desyncs overlap-add silently — the decoded samples
//! look plausible and are wrong, exactly the failure class this workspace's
//! other codec work keeps finding. [`sine_window_satisfies_princen_bradley`]
//! checks it directly; the unit tests below hold every size this crate is
//! meant to serve (2048 and 256) to it, not just the general formula's
//! algebraic identity (`sin²θ + cos²θ == 1`, which is what makes the sine
//! window satisfy Princen-Bradley in the first place: `i` and `i + N/2` land
//! `π/2` apart in the argument).
//!
//! # No allocation
//!
//! Every function here writes into a caller-provided buffer or returns a
//! fixed-size array (`sine_window::<N>()`, using `N` as a const generic) —
//! never a `Vec`. A window's length is one of a handful of values a codec's
//! own bitstream selects from a closed set (2048/256 for AAC), never a value
//! read directly off untrusted input, so there is no budget to charge this
//! against; the right fix is simply not to allocate.

/// Compute the `N`-sample sine window into a fixed-size array, entirely on
/// the stack.
///
/// `N` is a compile-time constant, which is the right shape for a caller that
/// already knows which of a codec's fixed window sizes it wants (AAC:
/// `sine_window::<2048>()` for a long block, `sine_window::<256>()` for one
/// of the eight short blocks) and does not want to size or bounds-check a
/// runtime buffer for something that is really a constant.
#[must_use]
pub fn sine_window<const N: usize>() -> [f32; N] {
    std::array::from_fn(|i| sample(i, N))
}

/// Compute the `n`-sample sine window into `out`, for callers whose window
/// length is only known at runtime (still one of a small closed set — a
/// codec's own window-sequence syntax element, not attacker-sized).
///
/// Writes `out.len().min(n)` samples starting at `out[0]` and returns that
/// count, so a buffer that is the wrong size is truncated rather than
/// panicking or reading/writing out of bounds. A caller that needs to detect
/// a size mismatch compares the return value against `out.len()` and `n`
/// itself.
pub fn sine_window_into(out: &mut [f32], n: usize) -> usize {
    let len = out.len().min(n);
    for (i, slot) in out.iter_mut().enumerate().take(len) {
        *slot = sample(i, n);
    }
    len
}

/// `sin( (π / n) * (i + 0.5) )`, computed in `f64` and rounded to `f32` at
/// the end — matching this workspace's usual convention (see
/// `vaco-codec-mpegaudio`'s synthesis window) of doing transcendental-function
/// work in `f64` even when the stored/consumed type is `f32`, so the
/// rounding error is the final store's alone rather than compounded through
/// the trig call too.
fn sample(i: usize, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let arg = std::f64::consts::PI / (n as f64) * (i as f64 + 0.5);
    arg.sin() as f32
}

/// Check the Princen-Bradley condition — `win[i]^2 + win[i + n/2]^2 == 1` for
/// every `i` in `0..n/2` — to within `tolerance`.
///
/// `n` must be even (every real window this crate serves is: 2048 and 256 are
/// both powers of two); an odd `n` has no well-defined "second half" sample to
/// pair `i` against and returns `true` vacuously (there is nothing to check),
/// which callers should treat as their own input error rather than a passed
/// check — this crate's own tests never call it with an odd `n`.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "n/2 is an exact halving of a window length that is always even in practice; truncation is not in play"
)]
pub fn sine_window_satisfies_princen_bradley(n: usize, tolerance: f32) -> bool {
    let half = n / 2;
    for i in 0..half {
        let a = sample(i, n);
        let b = sample(i + half, n);
        let sum = a.mul_add(a, b * b);
        if (sum - 1.0).abs() > tolerance {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code, fixed-size arrays")]
    use super::{sine_window, sine_window_into, sine_window_satisfies_princen_bradley};

    #[test]
    fn long_window_satisfies_princen_bradley() {
        assert!(sine_window_satisfies_princen_bradley(2048, 1e-5));
    }

    #[test]
    fn short_window_satisfies_princen_bradley() {
        assert!(sine_window_satisfies_princen_bradley(256, 1e-5));
    }

    #[test]
    fn the_window_is_symmetric() {
        // sin(π/N * (i+0.5)) and sin(π/N * (N-1-i+0.5)) = sin(π - π/N*(i+0.5))
        // are equal, since sin(π - x) == sin(x).
        let w = sine_window::<256>();
        for i in 0..256 {
            assert!(
                (w[i] - w[255 - i]).abs() < 1e-6,
                "index {i}: {} vs {}",
                w[i],
                w[255 - i]
            );
        }
    }

    #[test]
    fn every_sample_is_in_zero_one_inclusive() {
        // sin of an argument in (0, π) is in (0, 1]; never negative, never
        // above 1 — a window that could go negative or exceed unity would be
        // a wrong formula, not a rounding artifact.
        let w = sine_window::<2048>();
        for &v in &w {
            assert!((0.0..=1.0).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn a_tiny_window_matches_the_formula_computed_independently() {
        // N=4 recomputed by hand from the formula, as a second, independent
        // check that `sample`'s implementation matches its own doc comment —
        // small enough to read every value at a glance.
        let w = sine_window::<4>();
        let expected = [
            (std::f64::consts::PI / 4.0 * 0.5).sin() as f32,
            (std::f64::consts::PI / 4.0 * 1.5).sin() as f32,
            (std::f64::consts::PI / 4.0 * 2.5).sin() as f32,
            (std::f64::consts::PI / 4.0 * 3.5).sin() as f32,
        ];
        for i in 0..4 {
            assert!((w[i] - expected[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn sine_window_into_truncates_to_the_shorter_of_buffer_and_n_without_panicking() {
        let mut buf = [0.0f32; 4];
        let written = sine_window_into(&mut buf, 8);
        assert_eq!(written, 4);
        // Same first four samples as an N=8 window, not an N=4 window.
        for (i, slot) in buf.iter().enumerate() {
            let expected = (std::f64::consts::PI / 8.0 * (i as f64 + 0.5)).sin() as f32;
            assert!((slot - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn zero_length_window_never_panics() {
        assert_eq!(sine_window_into(&mut [], 0), 0);
        let w = sine_window::<0>();
        assert_eq!(w.len(), 0);
    }
}
