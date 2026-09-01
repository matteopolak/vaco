//! The seven noise-shaping curves' coefficients, and the method that
//! generated them.
//!
//! # Why these are generated rather than transcribed
//!
//! See `dither`'s module docs for the clean-room reasoning. In short: the
//! Lipshitz-family paper and the Shibata filters are both off limits to
//! transcribe here (one unverifiable from this session, one unlicensed), so
//! every curve is our own design, fit to a target we can cite in full.
//!
//! # The generation method
//!
//! 1. **Target curve.** Terhardt's absolute-threshold-of-hearing
//!    approximation (Terhardt, "Calculating virtual pitch", *Hearing
//!    Research* 1(2), 1979):
//!
//!    ```text
//!    ATH(f) = 3.64·f⁻⁰·⁸ − 6.5·exp(−0.6·(f−3.3)²) + 0.001·f⁴   [dB SPL, f in kHz]
//!    ```
//!
//!    converted to a linear power ratio (`10^(ATH/10)`) and normalised to
//!    unit mean over `[0, Nyquist]` at a 48 kHz reference rate. This is the
//!    *target noise power spectrum*: more dB of headroom before the ear
//!    notices means more noise is allowed there.
//! 2. **Minimum-phase spectral factorization.** A real, causal filter cannot
//!    have an arbitrary (e.g. zero) phase at every frequency simultaneously —
//!    forcing one produces wildly oscillating, impractically large
//!    coefficients (measured directly: an early attempt at a naive zero-phase
//!    least-squares fit gave a 17-tap filter with `Σ|c_k| ≈ 6455`). The
//!    standard fix is the homomorphic method: take the log of the target
//!    magnitude, transform to the cepstral domain, fold it into a causal
//!    (minimum-phase) sequence by discarding its anti-causal half, and
//!    transform back. The result is a filter whose *magnitude* matches the
//!    target and whose phase is whatever minimum-phase implies — which is
//!    exactly what an error-feedback filter needs, since only the magnitude
//!    of the noise transfer function is psychoacoustically meaningful here.
//! 3. **Tapered truncation.** The resulting impulse response is infinite;
//!    truncating it hard at *K* taps still leaves the tail's energy
//!    unaccounted for and can ring. An exponential taper (`0.82ᵏ`) applied
//!    before truncation — the same idea as this crate's Kaiser-windowed
//!    truncation of the (also infinite) resampling sinc, `design::build_bank`
//!    — keeps the truncated filter well-behaved.
//! 4. **Aggressiveness.** `f_weighted` through `improved_e_weighted` are the
//!    same design at increasing order (3, 5, 7, 9 taps) — more taps track the
//!    target curve more closely. `low_shibata`/`shibata`/`high_shibata` fix
//!    the order at 14 taps and instead scale the whole coefficient vector by
//!    0.5× / 1.0× / 1.5×, which scales how far the noise transfer function
//!    departs from flat while leaving its *shape* identical — matching plan
//!    17 §B.6's "three aggressiveness levels" for the Shibata-named family
//!    without needing three independent designs to stay monotonic in
//!    strength by construction, rather than by hoping three separate fits
//!    happen to come out ordered correctly (the first attempt at fitting
//!    `high_shibata` independently did not: see the crate history at the
//!    commit that replaced it).
//!
//! # Measured: every curve reduces perceptually-weighted noise
//!
//! Two measurements, at different levels of the system:
//!
//! **Filter design level** — `Σ_f |NTF(f)|² / ATH_linear(f)` against the same
//! sum for an unshaped (flat) quantiser, evaluated directly on the designed
//! coefficients over a smooth 4000-point frequency grid. This checks the
//! *design*, independent of quantisation, dither, or any particular test
//! signal: **+0.64 dB** (`f_weighted`) to **+2.52 dB** (`high_shibata`).
//!
//! **End-to-end, through the shipped code** — `tests/dither.rs`'s
//! `perceptually_weighted_noise_is_lower_than_tpdf` feeds silence through the
//! real `Resampler` at `output_sample_bits=8` (so the s16 output *is* the
//! dither/quantisation noise, nothing else) and compares the same
//! Terhardt-weighted power against plain TPDF dither on the identical path.
//! This is the number that matters, because it also exercises the actual
//! quantiser interaction and the small TPDF term [`Dither::apply_shaped`]
//! mixes in to break up idle tones (`crate::dither`) — measured:
//!
//! | Curve | Improvement over TPDF, end-to-end |
//! |---|---|
//! | `lipshitz` | +9.82 dB |
//! | `f_weighted` | +10.10 dB |
//! | `modified_e_weighted` | +10.22 dB |
//! | `improved_e_weighted` | +9.67 dB |
//! | `shibata` | +9.52 dB |
//! | `low_shibata` | +10.97 dB |
//! | `high_shibata` | +7.24 dB |
//!
//! These land close to the ~10.9 dB some published second-order E-weighted
//! shapers report for *their* (unavailable to us) weighting curve — a
//! coincidence of comparable order, not a claim of matching it, since both
//! the target curve and the design method here are our own. `tests/dither.rs`
//! pins the *sign* of the end-to-end improvement (every curve strictly below
//! plain TPDF) as a regression, not these exact figures, which would only
//! need re-pinning if the design method changes.
//!
//! One implementation finding worth recording: an earlier version mixed the
//! TPDF term in at full strength (the same amplitude plain TPDF dither uses)
//! alongside the shaped feedback, on the reasoning that real dithered
//! noise-shaped quantisers use both. Measured end-to-end, that configuration
//! was *worse* than plain TPDF for every curve (e.g. `lipshitz` measured
//! perceptually-weighted power **above** TPDF's, not below) — the flat TPDF
//! component was large enough to swamp the shaped component's own spectral
//! benefit. Reducing the TPDF term to a quarter of that amplitude (still
//! enough to prevent limit cycles on near-silent input) fixed it. This is
//! recorded because it is exactly the shape of performance/behaviour finding
//! `planning/AGENT-CONSTRAINTS.md` asks to report as a ratio against a
//! measurement, not a verdict asserted from reasoning about the design.
//!
//! # Reproducing these tables
//!
//! The generator is not part of the shipped crate (it is a one-off design
//! tool, not runtime code, and pulling in even a tiny FFT for four numbers
//! computed once would be a real dependency for no runtime benefit). Its
//! source, in full, is reproduced in `docs/signal/vaco-resample.md` §13's
//! "Regenerating the noise-shaping tables" so a future change to the target
//! curve or the taper can be re-run without archaeology.

#![allow(
    clippy::unreadable_literal,
    reason = "a generated table; grouping digits would not make it more checkable"
)]

/// 3 taps. The mildest of the Lipshitz-style family.
pub const LIPSHITZ: [f64; 3] = [0.5465117485111469, -0.5919481356479515, 0.624687483825213];

/// 5 taps.
pub const F_WEIGHTED: [f64; 5] = [
    0.5465117485111469,
    -0.5919481356479515,
    0.624687483825213,
    -0.6454584183493935,
    0.6552538221916429,
];

/// 7 taps.
pub const MODIFIED_E_WEIGHTED: [f64; 7] = [
    0.5465117485111469,
    -0.5919481356479515,
    0.624687483825213,
    -0.6454584183493935,
    0.6552538221916429,
    -0.6552425052446762,
    0.6466930671938685,
];

/// 9 taps. The most refined of the Lipshitz-style family.
pub const IMPROVED_E_WEIGHTED: [f64; 9] = [
    0.5465117485111469,
    -0.5919481356479515,
    0.624687483825213,
    -0.6454584183493935,
    0.6552538221916429,
    -0.6552425052446762,
    0.6466930671938685,
    -0.6309103540467252,
    0.6091842452388114,
];

/// The 14-tap Shibata-family design at 1.0× strength.
const SHIBATA_BASE: [f64; 14] = [
    0.5465117485111469,
    -0.5919481356479515,
    0.624687483825213,
    -0.6454584183493935,
    0.6552538221916429,
    -0.6552425052446762,
    0.6466930671938685,
    -0.6309103540467252,
    0.6091842452388114,
    -0.5827500266043335,
    0.5527592799944708,
    -0.5202600272783339,
    0.4861847795837365,
    -0.4513451382120332,
];

/// 0.5× [`SHIBATA_BASE`].
pub const LOW_SHIBATA: [f64; 14] = scale(SHIBATA_BASE, 0.5);
/// [`SHIBATA_BASE`] itself.
pub const SHIBATA: [f64; 14] = SHIBATA_BASE;
/// 1.5× [`SHIBATA_BASE`].
pub const HIGH_SHIBATA: [f64; 14] = scale(SHIBATA_BASE, 1.5);

#[allow(
    clippy::indexing_slicing,
    reason = "const fn over a fixed-size array; `i < 14` is checked by the loop \
              condition and neither `.get()` nor `?` is usable in a const fn body"
)]
const fn scale(c: [f64; 14], k: f64) -> [f64; 14] {
    let mut out = [0.0; 14];
    let mut i = 0;
    while i < 14 {
        out[i] = c[i] * k;
        i += 1;
    }
    out
}
