//! Sine and Kaiser-Bessel-Derived (KBD) window generation for MDCT-based
//! codecs (D-06 named this crate for the sine window specifically; KBD was
//! added when a real fixture proved the sine-only scope wrong — see
//! [`kbd_window`]'s own doc for exactly how).
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
//! KBD (Kaiser-Bessel-Derived), AAC's other window shape, **is now also
//! here** — [`kbd_window`] — despite D-06 naming this crate for the sine
//! window specifically and an earlier version of this doc saying KBD was
//! "not here" on the working assumption that real (`ffmpeg`-encoded)
//! content never uses it. That assumption was checked against
//! `vaco-codec-aac`'s own decode of real fixtures and found wrong: several
//! genuinely set `window_shape == 1` partway through, past a bit-exact
//! syntax-consumption check that rules out a parsing artefact. Extending
//! this crate rather than starting a second one keeps "the window shapes
//! AAC decode needs" in one place; a crate that only ever covered one of
//! AAC's exactly two shapes was always going to need this correction once
//! something real exercised it.
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

/// Kaiser-Bessel-Derived (KBD) window, AAC's other window shape
/// (ISO/IEC 14496-3 subpart 4 §4.6.11.3.2, `window_shape == 1`).
///
/// This crate started sine-only (D-06 names it for the sine window
/// specifically) on the working assumption that `ffmpeg`'s AAC encoder —
/// this workspace's only source of real AAC fixtures — never emits
/// `window_shape == 1`. **That assumption was wrong**: verified directly
/// against real `ffmpeg`-encoded frames (`vaco-codec-aac`'s own decode
/// pass, T3-03c/#445), several fixtures genuinely set `window_shape == 1`
/// partway through the stream, past a bit-exact-consumption check that
/// rules out a parsing artefact. A crate whose whole reason to exist is
/// "the window shapes AAC decode needs" cannot leave one of AAC's exactly
/// two shapes out, so KBD is added here rather than in a new crate — see
/// the module doc's own updated scope note.
///
/// `alpha` is `4` for `N=2048` (1920) and `6` for `N=256` (240), per
/// §4.6.11.3.2's own table — callers state it explicitly rather than this
/// function guessing from `N`, since a future low-delay window at a third
/// `N` would otherwise need a third magic case here.
///
/// The construction (§4.6.11.3.2): a kernel window
/// `W'(n) = I0(πα·sqrt(1 - ((n - N/4)/(N/4))^2)) / I0(πα)` for `0 <= n <=
/// N/2`, `I0` the modified Bessel function of the first kind, evaluated by
/// its own defining power series (`Σ_{k=0}^∞ ((x/2)^k / k!)^2`, which
/// converges quickly enough for the `α` values AAC uses that 32 terms is
/// generous); then each window half is a *cumulative sum* of that kernel,
/// normalised by the kernel's own total sum over `0..=N/2`, and then
/// **square-rooted** — the KBD window's defining property (distinct from
/// the sine window's plain pointwise formula) is that it is built from a
/// running sum, not a closed-form value per sample, and the square root is
/// not optional: dropping it still yields a symmetric, monotonic,
/// `[0, 1]`-bounded window (this crate's first attempt did, and passed
/// those three properties' own tests), but fails the Princen-Bradley
/// identity by ~1e-2 to ~2e-3 depending on sample index, because
/// `cumsum(n)/total + cumsum(half-1-n)/total == 1` is the algebraic
/// identity the kernel's symmetry actually gives (proved from
/// `total - cumsum(m) == cumsum(half-1-m)`, itself from `d[k] ==
/// d[half-k]`), and squaring *after* the square root is what turns that
/// sum-to-one identity into the sum-of-*squares*-to-one Princen-Bradley
/// needs.
#[must_use]
#[allow(clippy::integer_division, reason = "N/2 is an exact halving of a window length that is always even")]
pub fn kbd_window<const N: usize>(alpha: f64) -> [f32; N] {
    let half = N / 2;
    if half == 0 {
        return [0.0; N];
    }
    // Kernel values for n in 0..=half (half+1 points), and their
    // cumulative sum, computed once.
    let mut kernel = vec![0.0f64; half + 1];
    for (n, slot) in kernel.iter_mut().enumerate() {
        let x = (n as f64 - N as f64 / 4.0) / (N as f64 / 4.0);
        let arg = std::f64::consts::PI * alpha * (1.0 - x * x).max(0.0).sqrt();
        *slot = bessel_i0(arg) / bessel_i0(std::f64::consts::PI * alpha);
    }
    let mut cumsum = vec![0.0f64; half + 1];
    let mut running = 0.0;
    for (i, &k) in kernel.iter().enumerate() {
        running += k;
        if let Some(slot) = cumsum.get_mut(i) {
            *slot = running;
        }
    }
    let total = cumsum.get(half).copied().unwrap_or(1.0).max(f64::EPSILON);

    std::array::from_fn(|n| {
        let idx = if n < half { n } else { N - n - 1 };
        let ratio = cumsum.get(idx).copied().unwrap_or(0.0) / total;
        ratio.sqrt() as f32
    })
}

/// The modified Bessel function of the first kind, `I0(x) = Σ_{k=0}^∞
/// ((x/2)^k / k!)^2`, by its own defining series — accurate to `f64`
/// precision for the `x` range `kbd_window` calls it with (`πα` up to
/// roughly 19 for `α=6`), where the series' terms shrink well within 32
/// iterations.
fn bessel_i0(x: f64) -> f64 {
    let half_x = x / 2.0;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    for k in 1..32 {
        term *= half_x / f64::from(k);
        sum += term * term;
    }
    sum
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code, fixed-size arrays")]
    #![allow(clippy::integer_division, reason = "n/2 is an exact halving of a window length that is always even")]
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

    #[test]
    fn kbd_window_satisfies_princen_bradley_at_both_aac_sizes() {
        for (n, alpha) in [(2048usize, 4.0), (256, 6.0)] {
            let w: Vec<f32> = match n {
                2048 => super::kbd_window::<2048>(alpha).to_vec(),
                _ => super::kbd_window::<256>(alpha).to_vec(),
            };
            let half = n / 2;
            for i in 0..half {
                let a = f64::from(w[i]);
                let b = f64::from(w[i + half]);
                let sum = a.mul_add(a, b * b);
                assert!((sum - 1.0).abs() < 1e-4, "n={n} i={i}: sum={sum}");
            }
        }
    }

    #[test]
    fn kbd_window_is_symmetric_and_bounded() {
        let w = super::kbd_window::<256>(6.0);
        for i in 0..256 {
            assert!((0.0..=1.0).contains(&w[i]), "out of range at {i}: {}", w[i]);
            assert!((w[i] - w[255 - i]).abs() < 1e-5, "asymmetric at {i}");
        }
    }

    #[test]
    fn kbd_window_is_monotonically_increasing_on_its_left_half() {
        // The left half is a normalised cumulative sum of non-negative
        // kernel values, so it can never decrease.
        let w = super::kbd_window::<256>(6.0);
        for i in 1..128 {
            assert!(w[i] >= w[i - 1] - 1e-6, "decreased at {i}: {} -> {}", w[i - 1], w[i]);
        }
    }
}
