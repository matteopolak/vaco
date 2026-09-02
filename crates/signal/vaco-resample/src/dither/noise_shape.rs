//! The seven noise-shaping curves' coefficients, and the method that
//! generated them. Generated, not transcribed: the Lipshitz-family paper and
//! the Shibata filters are both off limits to transcribe (one unverifiable
//! from this session, one unlicensed), so every curve is our own design, fit
//! to a target we can cite in full — see `dither`'s module docs.
//!
//! Method: target curve is Terhardt's absolute-threshold-of-hearing
//! approximation (1979), normalised to unit mean over `[0, Nyquist]` at
//! 48 kHz. Minimum-phase spectral factorization (the standard homomorphic
//! method) avoids the impractically large coefficients a zero-phase fit
//! produces (measured: a naive attempt gave a 17-tap filter with
//! `Σ|c_k| ≈ 6455`). An exponential taper (`0.82ᵏ`) precedes truncation of
//! the infinite impulse response. `f_weighted` through `improved_e_weighted`
//! are the same design at increasing order (3/5/7/9 taps); `low_shibata`/
//! `shibata`/`high_shibata` fix the order at 14 taps and scale the
//! coefficients by 0.5×/1.0×/1.5×, monotonic by construction (an earlier
//! independent fit of `high_shibata` was not).
//!
//! # Measured: every curve reduces perceptually-weighted noise
//!
//! Design-level: **+0.64 dB** (`f_weighted`) to **+2.52 dB** (`high_shibata`).
//! End-to-end, via `tests/dither.rs`, against plain TPDF:
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
//! `tests/dither.rs` pins the *sign*, not these figures. Regression found
//! once: mixing the TPDF term in at full strength alongside shaped feedback
//! measured *worse* than plain TPDF for every curve — the flat component
//! swamped the shaped one's spectral benefit; a quarter of that amplitude
//! (still enough to prevent limit cycles on near-silent input) fixed it. The
//! generator is not part of the shipped crate; its full source is in
//! `docs/signal/vaco-resample.md` §13.

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
