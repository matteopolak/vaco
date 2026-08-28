//! RBJ Audio EQ Cookbook biquad coefficient design — the one definition
//! (D19) for every `vaco-filter-a*` crate that needs a two-pole IIR
//! section.
//!
//! `equalizer`, `bass`/`lowshelf`, `treble`/`highshelf`, `highpass`,
//! `lowpass`, `bandpass`, `bandreject`, `allpass` and the raw `biquad`
//! filter (all in `vaco-filter-aeq`) are one IIR section — `y = b0 x0 + b1
//! x1 + b2 x2 - a1 y1 - a2 y2` — with different coefficient formulas. Those
//! formulas come from Robert Bristow-Johnson's "Audio EQ Cookbook" (the
//! standard, citable reference for RBJ biquads; `provenance/sources.toml`
//! records it as `rbj-audio-eq-cookbook`), not from any implementation.
//!
//! # Why this moved here (D19, FT — filter crate consolidation)
//!
//! This module was originally `vaco-filter-aeq::engine`, `pub(crate)`
//! — private to that crate on the theory that only the EQ family would ever
//! need a biquad. That theory did not survive contact with the rest of
//! `planning/16-filters.md` §4.3: `vaco-filter-aeffects`, `-ameasure` and
//! `-audio-dynamics` each needed a two-pole section (a band splitter, a
//! K-weighting cascade, a crossover filter) and, finding the EQ crate's
//! version crate-private, each wrote its own — `-aeffects` fell back to
//! one-pole approximations for `aexciter`/`deesser`/`virtualbass` instead
//! (documented in those modules as "no cross-crate biquad access"), and
//! `-ameasure::kweight` and `-audio-dynamics::mcompand` duplicated the
//! cookbook formulas outright. Plan §4.1 already named "biquad coefficient
//! design" as one of this crate's shared kernels; this is that move.
//!
//! # Why the tests here are a real oracle
//!
//! A biquad coefficient set and a "reimplementation of the same formula"
//! agree by construction — two transcriptions of one sentence cannot
//! disagree (see `planning/AGENT-CONSTRAINTS.md`'s HEVC IDCT cautionary
//! tale). The property this module's tests check instead is the frequency
//! response itself: [`Coeffs::response_db`] evaluates `H(e^{jw})` directly
//! from the *z-transform definition*, not from the cookbook's algebra, so a
//! coefficient sign error or a wrong `Q`/`BW`/`S` mapping shows up as a
//! `-3 dB` point or a design-frequency gain that lands in the wrong place —
//! a route to the answer that is genuinely independent of how the
//! coefficients were derived. It is `pub`, not `#[cfg(test)]`, precisely so
//! every downstream crate's own tests get the same independent oracle
//! rather than re-deriving one.

use std::f64::consts::{LN_2, PI};

/// How `width` is interpreted. Probed via `ffmpeg -h filter=equalizer`
/// (2026-08-23): `width_type` takes `h`/`o`/`q`/`s`/`k`, default `q`.
///
/// * `h` (Hz) and `k` (kHz) give an absolute bandwidth; converted to a `Q` as
///   `Q = frequency / width_hz`, which is the conventional bandwidth-to-Q
///   relation and matches the reference's documented behaviour that `width`
///   in Hz shrinks as the passband narrows.
/// * `q` uses `width` directly as the cookbook's `Q`.
/// * `o` uses `width` as the cookbook's `BW` (bandwidth in octaves).
/// * `s` uses `width` as the cookbook's shelf slope `S` — meaningful only for
///   `lowshelf`/`highshelf`/`bass`/`treble`/`tiltshelf`; treated as `Q` for
///   any other filter that (unusually) selects it, since the cookbook defines
///   no shelf slope for a non-shelving section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidthType {
    Hz,
    KHz,
    #[default]
    QFactor,
    Octave,
    Slope,
}

impl WidthType {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "h" => Some(Self::Hz),
            "k" => Some(Self::KHz),
            "q" => Some(Self::QFactor),
            "o" => Some(Self::Octave),
            "s" => Some(Self::Slope),
            _ => None,
        }
    }
}

/// One biquad section's coefficients, normalised so `a0 == 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    /// Build from the six raw cookbook coefficients, dividing through by `a0`.
    ///
    /// `a0` non-finite or zero (a cutoff of 0 Hz or at/above Nyquist makes
    /// `sin`/`cos` degenerate and can drive `a0` to zero) yields the identity
    /// section rather than propagating `NaN`/`inf` into every sample this
    /// filter will ever see — see this crate's fuzz target and each caller's
    /// docs for the measurement. **This fallback is load-bearing: keep it,
    /// and keep the tests that pin it** — no parameter combination any
    /// caller passes may put a `NaN` into a sample stream.
    #[must_use]
    pub fn normalise(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        if !a0.is_finite() || a0 == 0.0 {
            return Self::identity();
        }
        let c = Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        };
        if c.b0.is_finite()
            && c.b1.is_finite()
            && c.b2.is_finite()
            && c.a1.is_finite()
            && c.a2.is_finite()
        {
            c
        } else {
            Self::identity()
        }
    }

    /// The no-op section: `y = x`.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// `H(e^{jw})` in dB, evaluated directly from the z-transform definition
    /// `H(z) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)`.
    ///
    /// This is the independent route to the response described in the
    /// module doc, not a helper the filters call in production — but it is
    /// a normal `pub fn`, not `#[cfg(test)]`, because the whole point of
    /// moving this module here is that other crates' *own* tests need this
    /// same oracle. Gating it to this crate's test builds would have kept
    /// it exactly as unreachable to callers as the `pub(crate)` visibility
    /// this move is fixing.
    #[must_use]
    pub fn response_db(&self, w: f64) -> f64 {
        let (re, im) = self.response(w);
        10.0 * (re.mul_add(re, im * im)).log10()
    }

    fn response(&self, w: f64) -> (f64, f64) {
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = self.b1 * s1 + self.b2 * s2;
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = self.a1 * s1 + self.a2 * s2;
        let den2 = den_re.mul_add(den_re, den_im * den_im);
        (
            (num_re * den_re + num_im * den_im) / den2,
            (num_im * den_re - num_re * den_im) / den2,
        )
    }
}

/// Per-channel filter state (Direct Form I): the last two inputs and outputs.
///
/// The reference exposes a `transform` option (`di`/`dii`/`tdi`/`tdii`/`latt`/
/// `svf`/`zdf`) that picks among numerically-different realisations of the
/// *same* transfer function. Direct Form I is implemented; the others are not
/// — they change rounding behaviour under fixed-point/`f32` precision, not the
/// `f64` response this crate computes in, so DI is a faithful (if not
/// bit-identical-to-every-mode) realisation.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl State {
    pub fn process(&mut self, c: &Coeffs, x0: f64) -> f64 {
        let y0 = c.b0.mul_add(
            x0,
            c.b1.mul_add(
                self.x1,
                c.b2.mul_add(self.x2, -c.a1.mul_add(self.y1, c.a2 * self.y2)),
            ),
        );
        self.x2 = self.x1;
        self.x1 = x0;
        self.y2 = self.y1;
        self.y1 = y0;
        y0
    }
}

/// `alpha`, the cookbook's shared intermediate, for every `width_type`.
///
/// `shelf` is unused by `WidthType::Slope` itself — probed directly
/// against the reference (`ffmpeg`'s `highpass=f=200:t=s:w=2` vs `t=q` vs
/// `t=o` on a raw `f64le` sine, three different md5 hashes) and confirmed
/// the reference does *not* collapse `Slope` into `Q`-factor outside the
/// shelving filters, unlike an earlier version of this function. The
/// reference's own `af_biquads.c` runs the same `S`-slope formula
/// (`alpha_shelf` here) for every filter regardless of shelf-ness, with
/// `A` (this function's `a_lin`) simply equal to `1.0` wherever a filter
/// has no gain parameter at all — `alpha_shelf` reduces to
/// `sin(w0)/2 * sqrt(2/S)` at `A = 1`, a well-defined formula that does not
/// need a shelving section to make sense. `shelf` is kept as a parameter
/// (rather than removed) only because every call site already threads it
/// through for readability at the call site; it no longer changes this
/// function's own behaviour.
#[must_use]
pub fn alpha(
    width_type: WidthType,
    w0: f64,
    frequency: f64,
    width: f64,
    a_lin: f64,
    shelf: bool,
) -> f64 {
    let _ = shelf;
    let q_from_hz = |bandwidth_hz: f64| {
        if bandwidth_hz <= 0.0 {
            return f64::INFINITY;
        }
        frequency / bandwidth_hz
    };
    match width_type {
        WidthType::Hz => alpha_q(w0, q_from_hz(width)),
        WidthType::KHz => alpha_q(w0, q_from_hz(width * 1000.0)),
        WidthType::Octave => alpha_bw(w0, width),
        WidthType::Slope => alpha_shelf(w0, a_lin, width),
        WidthType::QFactor => alpha_q(w0, width),
    }
}

#[must_use]
pub fn alpha_q(w0: f64, q: f64) -> f64 {
    if q <= 0.0 {
        return 0.0;
    }
    w0.sin() / (2.0 * q)
}

fn alpha_bw(w0: f64, bw_octaves: f64) -> f64 {
    let s = w0.sin();
    if s == 0.0 {
        return 0.0;
    }
    s * ((LN_2 / 2.0 * bw_octaves * w0 / s).sinh())
}

fn alpha_shelf(w0: f64, a: f64, s_slope: f64) -> f64 {
    if s_slope <= 0.0 {
        return 0.0;
    }
    let inner = (a + 1.0 / a) * (1.0 / s_slope - 1.0) + 2.0;
    (w0.sin() / 2.0) * inner.max(0.0).sqrt()
}

/// Angular frequency `w0 = 2*pi*f0/fs`, the cookbook's normalised design
/// frequency. Callers treat a non-finite or out-of-`(0, pi)` result as the
/// "coefficients go non-finite" case `normalise` catches — a cutoff of 0 Hz
/// or at/above Nyquist.
#[must_use]
pub fn w0_of(sample_rate: f64, frequency: f64) -> f64 {
    2.0 * PI * frequency / sample_rate
}

/// `A`, the cookbook's linear gain term, from a dB gain.
#[must_use]
pub fn a_of(gain_db: f64) -> f64 {
    10f64.powf(gain_db / 40.0)
}

#[must_use]
pub fn lowpass(fs: f64, f0: f64, wt: WidthType, width: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = alpha(wt, w0, f0, width, 1.0, false);
    let cw = w0.cos();
    Coeffs::normalise(
        (1.0 - cw) / 2.0,
        1.0 - cw,
        (1.0 - cw) / 2.0,
        1.0 + a,
        -2.0 * cw,
        1.0 - a,
    )
}

#[must_use]
pub fn highpass(fs: f64, f0: f64, wt: WidthType, width: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = alpha(wt, w0, f0, width, 1.0, false);
    let cw = w0.cos();
    let half = f64::midpoint(1.0, cw);
    Coeffs::normalise(half, -(1.0 + cw), half, 1.0 + a, -2.0 * cw, 1.0 - a)
}

/// `csg`: constant skirt gain (peak gain `Q`) when true, else constant 0 dB
/// peak gain. `ffmpeg -h filter=bandpass` documents the `csg` boolean.
#[must_use]
pub fn bandpass(fs: f64, f0: f64, wt: WidthType, width: f64, csg: bool) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = alpha(wt, w0, f0, width, 1.0, false);
    let cw = w0.cos();
    let b0 = if csg { w0.sin() / 2.0 } else { a };
    Coeffs::normalise(b0, 0.0, -b0, 1.0 + a, -2.0 * cw, 1.0 - a)
}

#[must_use]
pub fn bandreject(fs: f64, f0: f64, wt: WidthType, width: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = alpha(wt, w0, f0, width, 1.0, false);
    let cw = w0.cos();
    Coeffs::normalise(1.0, -2.0 * cw, 1.0, 1.0 + a, -2.0 * cw, 1.0 - a)
}

/// `order`: 1 gives a first-order all-pass (`ffmpeg -h filter=allpass`'s
/// `order` option, 1 or 2); the cookbook only specifies the second-order
/// section, so order 1 uses the textbook first-order all-pass
/// `H(z) = (a + z^-1) / (1 + a z^-1)` with `a` from the same `tan(w0/2)`
/// relation the cookbook's second-order form reduces to at `Q -> infinity`.
#[must_use]
pub fn allpass(fs: f64, f0: f64, wt: WidthType, width: f64, order: u8) -> Coeffs {
    let w0 = w0_of(fs, f0);
    if order == 1 {
        let t = (w0 / 2.0).tan();
        let a = (t - 1.0) / (t + 1.0);
        return Coeffs::normalise(a, 1.0, 0.0, 1.0, a, 0.0);
    }
    let a = alpha(wt, w0, f0, width, 1.0, false);
    let cw = w0.cos();
    Coeffs::normalise(1.0 - a, -2.0 * cw, 1.0 + a, 1.0 + a, -2.0 * cw, 1.0 - a)
}

/// Peaking EQ: `equalizer`.
#[must_use]
pub fn peaking(fs: f64, f0: f64, wt: WidthType, width: f64, gain_db: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = a_of(gain_db);
    let al = alpha(wt, w0, f0, width, a, false);
    let cw = w0.cos();
    Coeffs::normalise(
        1.0 + al * a,
        -2.0 * cw,
        1.0 - al * a,
        1.0 + al / a,
        -2.0 * cw,
        1.0 - al / a,
    )
}

/// Low shelf: `bass`/`lowshelf`.
#[must_use]
pub fn lowshelf(fs: f64, f0: f64, wt: WidthType, width: f64, gain_db: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = a_of(gain_db);
    let al = alpha(wt, w0, f0, width, a, true);
    let cw = w0.cos();
    let sqrt_a_2al = 2.0 * a.sqrt() * al;
    Coeffs::normalise(
        a * ((a + 1.0) - (a - 1.0) * cw + sqrt_a_2al),
        2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
        a * ((a + 1.0) - (a - 1.0) * cw - sqrt_a_2al),
        (a + 1.0) + (a - 1.0) * cw + sqrt_a_2al,
        -2.0 * ((a - 1.0) + (a + 1.0) * cw),
        (a + 1.0) + (a - 1.0) * cw - sqrt_a_2al,
    )
}

/// High shelf: `treble`/`highshelf`.
#[must_use]
pub fn highshelf(fs: f64, f0: f64, wt: WidthType, width: f64, gain_db: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = a_of(gain_db);
    let al = alpha(wt, w0, f0, width, a, true);
    let cw = w0.cos();
    let sqrt_a_2al = 2.0 * a.sqrt() * al;
    Coeffs::normalise(
        a * ((a + 1.0) + (a - 1.0) * cw + sqrt_a_2al),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
        a * ((a + 1.0) + (a - 1.0) * cw - sqrt_a_2al),
        (a + 1.0) - (a - 1.0) * cw + sqrt_a_2al,
        2.0 * ((a - 1.0) - (a + 1.0) * cw),
        (a + 1.0) - (a - 1.0) * cw - sqrt_a_2al,
    )
}

/// One-pole low-pass (`poles=1`): `H(z) = (1-a) / (1 - a z^-1)`, `a = e^{-w0}`.
///
/// Structural: the reference's `poles=1` mode is a first-order IIR section,
/// but the cookbook (which covers only the two-pole case) is not the source
/// for this formula — a standard exponential-smoothing one-pole design is,
/// which is why this is not held to the same `-3 dB`-at-`f0` bar the
/// two-pole `lowpass`/`highpass` tests assert. Unity at DC and monotonic
/// roll-off are checked instead.
#[must_use]
pub fn lowpass_one_pole(fs: f64, f0: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = (-w0).exp();
    Coeffs::normalise(1.0 - a, 0.0, 0.0, 1.0, -a, 0.0)
}

/// One-pole high-pass (`poles=1`): the complement of [`lowpass_one_pole`],
/// `H_hp(z) = 1 - H_lp(z) = a(1 - z^-1) / (1 - a z^-1)`.
#[must_use]
pub fn highpass_one_pole(fs: f64, f0: f64) -> Coeffs {
    let w0 = w0_of(fs, f0);
    let a = (-w0).exp();
    Coeffs::normalise(a, -a, 0.0, 1.0, -a, 0.0)
}

/// `tiltshelf`'s pair of sections: a low shelf cutting `-gain/2` cascaded
/// with a high shelf boosting `+gain/2`, both centred at `f0`.
///
/// Not in the cookbook (which has no "tilt" filter), but a standard
/// construction from cookbook shelves: each stage crosses 0 dB at `f0`, so
/// cascading them sums to exactly `-gain/2` at DC and `+gain/2` at Nyquist
/// with nothing left at the pivot — a genuine tilt rather than a shelf.
/// Verified by [`tests::tiltshelf_pivots_between_the_two_gains`].
#[must_use]
pub fn tilt(fs: f64, f0: f64, wt: WidthType, width: f64, gain_db: f64) -> (Coeffs, Coeffs) {
    (
        lowshelf(fs, f0, wt, width, -gain_db / 2.0),
        highshelf(fs, f0, wt, width, gain_db / 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn w_of(fs: f64, hz: f64) -> f64 {
        2.0 * PI * hz / fs
    }

    #[test]
    fn lowpass_minus_3db_at_cutoff() {
        // Cookbook LPF/HPF at Q = 1/sqrt(2) (Butterworth) is exactly -3dB at f0.
        let c = lowpass(
            FS,
            1000.0,
            WidthType::QFactor,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let db = c.response_db(w_of(FS, 1000.0));
        assert!((db - (-3.0)).abs() < 0.05, "got {db} dB at cutoff");
    }

    #[test]
    fn lowpass_dc_gain_is_unity() {
        let c = lowpass(
            FS,
            1000.0,
            WidthType::QFactor,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let db = c.response_db(0.0);
        assert!(db.abs() < 0.01, "got {db} dB at DC");
    }

    #[test]
    fn highpass_minus_3db_at_cutoff() {
        let c = highpass(
            FS,
            1000.0,
            WidthType::QFactor,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let db = c.response_db(w_of(FS, 1000.0));
        assert!((db - (-3.0)).abs() < 0.05, "got {db} dB at cutoff");
    }

    #[test]
    fn highpass_nyquist_gain_is_unity() {
        let c = highpass(
            FS,
            1000.0,
            WidthType::QFactor,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let db = c.response_db(PI - 1e-6);
        assert!(db.abs() < 0.05, "got {db} dB at Nyquist");
    }

    #[test]
    fn bandpass_csg_peak_gain_is_q() {
        let q = 2.0;
        let c = bandpass(FS, 1000.0, WidthType::QFactor, q, true);
        let db = c.response_db(w_of(FS, 1000.0));
        let expect = 20.0 * q.log10();
        assert!((db - expect).abs() < 0.05, "got {db} dB, expected {expect}");
    }

    #[test]
    fn bandpass_no_csg_peak_gain_is_0db() {
        let c = bandpass(FS, 1000.0, WidthType::QFactor, 2.0, false);
        let db = c.response_db(w_of(FS, 1000.0));
        assert!(db.abs() < 0.05, "got {db} dB at design frequency");
    }

    #[test]
    fn bandreject_notch_is_deep() {
        let c = bandreject(FS, 1000.0, WidthType::QFactor, 2.0);
        let db = c.response_db(w_of(FS, 1000.0));
        assert!(db < -40.0, "got {db} dB at the notch");
    }

    #[test]
    fn bandreject_dc_and_nyquist_are_unity() {
        let c = bandreject(FS, 1000.0, WidthType::QFactor, 2.0);
        assert!(c.response_db(0.0).abs() < 0.05);
        assert!(c.response_db(PI - 1e-6).abs() < 0.05);
    }

    #[test]
    fn allpass_magnitude_is_flat() {
        let c = allpass(FS, 1000.0, WidthType::QFactor, 0.7, 2);
        for hz in [50.0, 500.0, 1000.0, 5000.0, 15000.0] {
            let db = c.response_db(w_of(FS, hz));
            assert!(db.abs() < 0.05, "allpass not flat at {hz} Hz: {db} dB");
        }
    }

    #[test]
    fn allpass_order1_is_also_flat() {
        let c = allpass(FS, 1000.0, WidthType::QFactor, 0.7, 1);
        for hz in [50.0, 1000.0, 15000.0] {
            let db = c.response_db(w_of(FS, hz));
            assert!(
                db.abs() < 0.05,
                "order-1 allpass not flat at {hz} Hz: {db} dB"
            );
        }
    }

    #[test]
    fn peaking_gain_at_design_frequency() {
        for gain in [-12.0, -3.0, 6.0, 12.0] {
            let c = peaking(FS, 1000.0, WidthType::QFactor, 1.0, gain);
            let db = c.response_db(w_of(FS, 1000.0));
            assert!((db - gain).abs() < 0.05, "gain {gain}: got {db} dB");
        }
    }

    #[test]
    fn peaking_zero_gain_is_identity() {
        let c = peaking(FS, 1000.0, WidthType::QFactor, 1.0, 0.0);
        for hz in [50.0, 1000.0, 15000.0] {
            let db = c.response_db(w_of(FS, hz));
            assert!(db.abs() < 1e-6, "0 dB peaking not identity at {hz}: {db}");
        }
    }

    #[test]
    fn lowshelf_dc_gain_matches_setting() {
        for gain in [-12.0, 6.0, 12.0] {
            let c = lowshelf(FS, 1000.0, WidthType::QFactor, 0.5, gain);
            let db = c.response_db(0.0);
            assert!((db - gain).abs() < 0.05, "gain {gain}: got {db} dB at DC");
        }
    }

    #[test]
    fn lowshelf_nyquist_is_unity() {
        let c = lowshelf(FS, 1000.0, WidthType::QFactor, 0.5, 12.0);
        let db = c.response_db(PI - 1e-6);
        assert!(db.abs() < 0.1, "got {db} dB at Nyquist");
    }

    #[test]
    fn highshelf_nyquist_gain_matches_setting() {
        for gain in [-12.0, 6.0, 12.0] {
            let c = highshelf(FS, 1000.0, WidthType::QFactor, 0.5, gain);
            let db = c.response_db(PI - 1e-6);
            assert!(
                (db - gain).abs() < 0.05,
                "gain {gain}: got {db} dB at Nyquist"
            );
        }
    }

    #[test]
    fn highshelf_dc_is_unity() {
        let c = highshelf(FS, 1000.0, WidthType::QFactor, 0.5, 12.0);
        let db = c.response_db(0.0);
        assert!(db.abs() < 0.1, "got {db} dB at DC");
    }

    #[test]
    fn zero_hz_cutoff_does_not_produce_nan() {
        // The failure mode this exists to prevent: a 0 Hz or above-Nyquist
        // cutoff must not let NaN/inf reach the coefficients.
        for f in [0.0, -10.0, FS, FS * 10.0] {
            for wt in [WidthType::QFactor, WidthType::Hz, WidthType::Octave] {
                let c = lowpass(FS, f, wt, 0.707);
                assert!(c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite());
                assert!(c.a1.is_finite() && c.a2.is_finite());
                let c = highpass(FS, f, wt, 0.707);
                assert!(c.b0.is_finite() && c.a1.is_finite());
                let c = peaking(FS, f, wt, 1.0, 6.0);
                assert!(c.b0.is_finite() && c.a1.is_finite());
            }
        }
    }

    #[test]
    fn zero_width_does_not_produce_nan() {
        for c in [
            peaking(FS, 1000.0, WidthType::QFactor, 0.0, 6.0),
            lowpass(FS, 1000.0, WidthType::Hz, 0.0),
            lowshelf(FS, 1000.0, WidthType::Slope, 0.0, 6.0),
        ] {
            assert!(c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite());
            assert!(c.a1.is_finite() && c.a2.is_finite());
        }
    }

    /// `WidthType::Slope` on a non-shelving filter used to fall back to
    /// treating `width` as `Q` (same as `QFactor`) -- probed against the
    /// real reference and confirmed wrong: `ffmpeg`'s own `af_biquads.c`
    /// runs the unconditional `S`-slope formula for every filter, shelving
    /// or not, with `A = 1` wherever there is no gain parameter at all.
    /// `alpha_shelf(w0, 1.0, s)` reduces to `sin(w0)/2 * sqrt(2/s)`, a
    /// different curve from `alpha_q(w0, s)` for `s != 1`, so a highpass at
    /// `t=s:w=2` must not produce the same coefficients as `t=q:w=2`.
    #[test]
    fn slope_on_a_non_shelving_filter_is_not_the_same_as_qfactor() {
        let via_slope = highpass(FS, 1000.0, WidthType::Slope, 2.0);
        let via_q = highpass(FS, 1000.0, WidthType::QFactor, 2.0);
        assert!(
            (via_slope.b0 - via_q.b0).abs() > 1e-6,
            "Slope collapsed into QFactor: {via_slope:?} vs {via_q:?}"
        );
        // Matches the reference's own unconditional `S`-slope formula
        // (`af_biquads.c`'s `SLOPE` case, run for every filter regardless
        // of shelf-ness, `A = 1` when there is no gain parameter) exactly,
        // not just "differs from QFactor" by coincidence.
        let w0 = w0_of(FS, 1000.0);
        let expected_alpha = alpha_shelf(w0, 1.0, 2.0);
        let via_alpha = alpha(WidthType::Slope, w0, 1000.0, 2.0, 1.0, false);
        assert!((via_alpha - expected_alpha).abs() < 1e-12);
    }

    #[test]
    fn one_pole_lowpass_dc_unity_and_rolls_off() {
        let c = lowpass_one_pole(FS, 1000.0);
        assert!(c.response_db(0.0).abs() < 0.01, "not unity at DC");
        let low = c.response_db(w_of(FS, 500.0));
        let high = c.response_db(w_of(FS, 10_000.0));
        assert!(
            high < low,
            "one-pole lowpass should attenuate more at 10kHz than 500Hz"
        );
    }

    #[test]
    fn one_pole_highpass_blocks_dc() {
        let c = highpass_one_pole(FS, 1000.0);
        assert!(c.response_db(0.0) < -40.0, "should attenuate heavily at DC");
        let low = c.response_db(w_of(FS, 100.0));
        let high = c.response_db(w_of(FS, 10_000.0));
        assert!(
            high > low,
            "one-pole highpass should pass 10kHz more than 100Hz"
        );
    }

    #[test]
    fn tiltshelf_pivots_between_the_two_gains() {
        let gain = 12.0;
        let (low, high) = tilt(FS, 1000.0, WidthType::QFactor, 0.5, gain);
        // Cascaded sections: dB responses add.
        let at = |hz: f64| low.response_db(w_of(FS, hz)) + high.response_db(w_of(FS, hz));
        assert!(
            (at(0.0) - (-gain / 2.0)).abs() < 0.1,
            "DC should read -gain/2"
        );
        assert!(
            (at(FS / 2.0 - 1.0) - (gain / 2.0)).abs() < 0.2,
            "Nyquist should read +gain/2"
        );
    }

    #[test]
    fn state_process_is_finite_for_finite_coeffs_and_input() {
        let c = peaking(FS, 1000.0, WidthType::QFactor, 1.0, 6.0);
        let mut s = State::default();
        for i in 0..1000 {
            let x = (f64::from(i) * 0.01).sin();
            let y = s.process(&c, x);
            assert!(y.is_finite());
        }
    }

    proptest::proptest! {
        /// Keeps the identity-section guarantee pinned: `Coeffs::identity()`
        /// run through `State` must reproduce any finite input sample for
        /// sample, for any sequence of inputs.
        #[test]
        fn identity_coeffs_never_change_the_signal(xs in proptest::collection::vec(-1.0e6f64..1.0e6, 0..64)) {
            let c = Coeffs::identity();
            let mut s = State::default();
            for x in xs {
                let y = s.process(&c, x);
                proptest::prop_assert!((y - x).abs() < 1e-9);
            }
        }

        /// `equalizer`'s cookbook peaking section at 0 dB gain is the
        /// identity for any design frequency/width the option ranges allow,
        /// not just the one case `peaking_zero_gain_is_identity` fixes.
        #[test]
        fn peaking_zero_gain_is_identity_for_any_design(
            f0 in 20.0f64..20_000.0,
            width in 0.1f64..10.0,
            probe_hz in 20.0f64..20_000.0,
        ) {
            let c = peaking(FS, f0, WidthType::QFactor, width, 0.0);
            let db = c.response_db(w_of(FS, probe_hz));
            proptest::prop_assert!(db.abs() < 1e-6, "got {} dB", db);
        }

        /// `bass`/`lowshelf` and `treble`/`highshelf` at 0 dB gain must also
        /// be the identity, checked across the option ranges rather than one
        /// fixed point.
        #[test]
        fn shelf_zero_gain_is_identity_for_any_design(
            f0 in 20.0f64..20_000.0,
            width in 0.1f64..10.0,
            probe_hz in 20.0f64..20_000.0,
        ) {
            let low = lowshelf(FS, f0, WidthType::QFactor, width, 0.0);
            let high = highshelf(FS, f0, WidthType::QFactor, width, 0.0);
            let w = w_of(FS, probe_hz);
            proptest::prop_assert!(low.response_db(w).abs() < 1e-6);
            proptest::prop_assert!(high.response_db(w).abs() < 1e-6);
        }

        /// Every coefficient this module can produce, across the full
        /// documented option ranges, stays finite — the fuzz target's
        /// property (no cutoff/width combination should let `NaN`/`inf`
        /// reach a sample) restated as a property test over the same space.
        #[test]
        fn coefficients_are_always_finite(
            f0 in -1000.0f64..100_000.0,
            width in -10.0f64..1000.0,
            gain_db in -900.0f64..900.0,
        ) {
            for c in [
                lowpass(FS, f0, WidthType::QFactor, width),
                highpass(FS, f0, WidthType::QFactor, width),
                bandpass(FS, f0, WidthType::QFactor, width, true),
                bandpass(FS, f0, WidthType::QFactor, width, false),
                bandreject(FS, f0, WidthType::QFactor, width),
                allpass(FS, f0, WidthType::QFactor, width, 2),
                peaking(FS, f0, WidthType::QFactor, width, gain_db),
                lowshelf(FS, f0, WidthType::QFactor, width, gain_db),
                highshelf(FS, f0, WidthType::QFactor, width, gain_db),
            ] {
                proptest::prop_assert!(c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite());
                proptest::prop_assert!(c.a1.is_finite() && c.a2.is_finite());
            }
        }
    }
}
