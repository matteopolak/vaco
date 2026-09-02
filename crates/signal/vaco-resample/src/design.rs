//! Polyphase filter design: windows, the anti-alias factor, and the bank.
//! The design was recovered, not guessed.
//!
//! For `48 kHz → 96 kHz` the reduced ratio is `1/2`: the bank has exactly two
//! phases, phase 0 comes back as an *exact* unit impulse (only `factor = 1`
//! makes `sinc` vanish at every non-zero integer), and fitting the remaining
//! 32 taps of phase 1 to full `f64` precision gives, to 5e-11:
//!
//! ```text
//! h[φ][j] = w( t / (T/2) ) · factor · sinc( factor · t ),   t = j − (T/2 − 1) − φ/P
//! ```
//!
//! with `w` a Kaiser window at `β = 9`. Repeating the fit while downsampling
//! (`48 → 32 kHz`, ratio `2/3`) supplies two pieces upsampling can't show:
//! `factor = min(1, (out/in) · cutoff)`, not `cutoff · min(1, out/in)` — at
//! `48 → 96` the latter gives 0.97 and phase 0 stops being an impulse, and
//! `cutoff=0.5` at `48 → 96` confirms it by producing byte-identical output
//! to the default (both clamp to 1); and the tap count grows as the filter
//! stretches, `T' = ceil(T / factor)` rounded up to even with the window
//! spanning the full `T'` (measured support of a downsampled impulse is 49.5
//! input samples for `T = 32, factor = 0.6467`, i.e. `T/factor`).
//!
//! One thing that is **not** there: per-phase normalisation. The whole bank
//! is scaled by `1 / Σ_j h[0][j]`, so phase 1 of the `1:2` bank sums to
//! `0.9999891346` rather than to 1 — the reference does not normalise
//! per-phase, and doing so would change every coefficient.
//!
//! # Accuracy of the reconstruction
//!
//! Measured against `FFmpeg` 8.1 on random signal, `f64` end to end:
//!
//! | | SNR |
//! |---|---|
//! | any upsampling ratio (`factor` clamps to 1) | **≈ 305 dB — bit-exact in `f64`** |
//! | downsampling (`factor < 1`) | 100–138 dB |
//!
//! The downsampling residual is a shape difference of about 4e-6 in the tap
//! weights that neither the tap count, the window span nor the `f32`/`f64`
//! rounding of `cutoff` accounts for — well inside the ≥ 100 dB fallback this
//! crate targets, and recorded rather than papered over.

#![allow(
    clippy::integer_division,
    reason = "tap and phase counts are non-zero by construction"
)]

use vaco_core::Error;
use vaco_limits::Budget;

use crate::convert::Internal;

/// Largest filter length we will build, before the `1/factor` stretch.
pub const MAX_FILTER_SIZE: usize = 65536;

/// Hard cap on the stretched tap count.
///
/// `taps = ceil(filter_size / factor)`, and `factor` shrinks with both the
/// downsampling ratio and `cutoff` — so `filter_size = 65536` with
/// `cutoff = 0.004` at `192000 -> 8000` asks for a filter of two billion taps.
/// The allocation budget would refuse it eventually; this refuses it before the
/// multiplication, with an error that names the option rather than the byte
/// count.
pub const MAX_TAPS: usize = 1 << 20;

/// How far the anti-alias stretch may lengthen a filter.
///
/// `1/factor` is unbounded as `cutoff` goes to zero, but a filter 256 times
/// longer than the caller asked for is not the filter they asked for. This
/// bounds the work *per output sample*, which the allocation budget does not:
/// a one-phase bank of a million taps allocates 4 MB and then costs a million
/// multiply-accumulates for every sample it produces.
pub const MAX_STRETCH: usize = 256;

/// Fuel charged per coefficient, before the bank is allocated.
///
/// A coefficient is a Bessel series (about thirty terms) plus a `sin`, so the
/// cost of *generating* a bank is not proportional to its size in bytes — a
/// 16 MiB bank that fits the allocation cap comfortably still takes a fifth of
/// a second to fill. Found by the fuzzer, which flagged a slow unit rather than
/// a crash. Charging fuel is the mechanism `vaco-limits` provides for exactly
/// this: work that is bounded but not by memory.
const FUEL_PER_COEFFICIENT: u64 = 32;

/// The reference's default `cutoff`, measured.
///
/// `48 → 32 kHz` puts the phase-0 centre tap at `0.6466729`, and
/// `0.6466729 · 3/2 = 0.9700093` — 0.97 plus the tiny excess the phase-0
/// normalisation introduces. Setting `cutoff=0.9` moves it to `0.9000094`,
/// which is the same excess on a different base.
pub const DEFAULT_CUTOFF: f64 = 0.97;

/// The reference's default `kaiser_beta`, measured: fitting the 32 taps of
/// phase 1 of the `1:2` bank gives `β = 9.000000000001` against a search over
/// `[8, 10]`, with a residual of 5e-11 — the precision the probe was recorded at.
pub const DEFAULT_KAISER_BETA: f64 = 9.0;

/// The window applied to the sinc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Window {
    /// Nuttall's 4-term minimum-sidelobe window (Nuttall 1981).
    BlackmanNuttall,
    /// Kaiser's `I₀`-sinh window (Kaiser 1974). The reference's default.
    #[default]
    Kaiser,
    /// Catmull–Rom, a 4-tap piecewise cubic. Not a windowed sinc at all.
    Cubic,
}

/// `sinc(x) = sin(πx)/(πx)`, with `sinc(0) = 1`.
///
/// The integer case is returned exactly rather than computed. `sin(π·n)/(π·n)`
/// evaluates to about `4e-17` rather than zero, because `PI` is not π — and the
/// reference's bank *does* have exact zeros: phase 0 of the `1:2` bank comes
/// back from the probe as an exact unit impulse, every other tap `0.0` and not
/// `4e-17`. Returning the mathematical value is both more accurate and what
/// reproduces that.
#[must_use]
pub fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    if x.fract() == 0.0 {
        return 0.0;
    }
    let px = core::f64::consts::PI * x;
    px.sin() / px
}

/// Zeroth-order modified Bessel function of the first kind.
///
/// `I₀(x) = Σ_{k≥0} ((x/2)^k / k!)²`, summed until the term falls below `1e-18`
/// relative — under 30 terms for `β ≤ 16`. This is init-time code, so the cost
/// does not matter and the series is the clearest form.
#[must_use]
pub fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0_f64;
    let mut term = 1.0_f64;
    let half = x * 0.5;
    for k in 1..200_u32 {
        let t = half / f64::from(k);
        term *= t * t;
        sum += term;
        if term < 1e-18 * sum {
            break;
        }
    }
    sum
}

/// Kaiser window over `u ∈ [−1, 1]`; zero outside.
#[must_use]
pub fn kaiser(u: f64, beta: f64) -> f64 {
    let v = 1.0 - u * u;
    if v <= 0.0 {
        0.0
    } else {
        bessel_i0(beta * v.sqrt()) / bessel_i0(beta)
    }
}

/// Blackman–Nuttall over `u ∈ [−1, 1]` (Nuttall 1981, −98 dB peak sidelobe).
#[must_use]
pub fn blackman_nuttall(u: f64) -> f64 {
    use core::f64::consts::PI;
    let p = f64::midpoint(u, 1.0);
    0.363_581_9 - 0.489_177_5 * (2.0 * PI * p).cos() + 0.136_599_5 * (4.0 * PI * p).cos()
        - 0.010_641_1 * (6.0 * PI * p).cos()
}

/// Catmull–Rom cubic, `(B, C) = (0, 0.5)`, over `|t| < 2`.
#[must_use]
pub fn catmull_rom(t: f64) -> f64 {
    let a = t.abs();
    if a < 1.0 {
        ((1.5 * a - 2.5) * a) * a + 1.0
    } else if a < 2.0 {
        (((-0.5 * a + 2.5) * a) - 4.0) * a + 2.0
    } else {
        0.0
    }
}

/// A coefficient bank: `phases × taps`, phase-major so one output sample reads
/// one contiguous run.
#[derive(Clone, Debug)]
pub struct Bank<T> {
    pub phases: usize,
    pub taps: usize,
    /// `taps/2 − 1`: the group delay, in input samples.
    pub centre: usize,
    coeffs: Vec<T>,
}

impl<T: Internal> Bank<T> {
    /// The taps of one phase, contiguous.
    #[must_use]
    pub fn phase(&self, phase: usize) -> Option<&[T]> {
        let start = phase.checked_mul(self.taps)?;
        self.coeffs.get(start..start.checked_add(self.taps)?)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.coeffs
    }
}

/// Parameters that determine a bank.
#[derive(Clone, Copy, Debug)]
pub struct DesignParams {
    /// Number of phases. `1` means "integer positions only".
    pub phases: usize,
    /// `filter_size`, before the downsampling stretch.
    pub filter_size: usize,
    /// `min(1, (out/in) · cutoff)`. See the module docs.
    pub factor: f64,
    pub window: Window,
    pub kaiser_beta: f64,
}

impl DesignParams {
    /// The anti-alias factor, as measured.
    ///
    /// `min(1, ratio · cutoff)` — the `min` is outside the product, so an
    /// upsampling conversion has no cutoff applied at all.
    #[must_use]
    pub fn factor(in_rate: u32, out_rate: u32, cutoff: f64) -> f64 {
        if in_rate == 0 || out_rate == 0 {
            return 1.0;
        }
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        (ratio * cutoff).clamp(f64::MIN_POSITIVE, 1.0)
    }

    /// The stretched tap count: `ceil(filter_size / factor)`, rounded up to even.
    #[must_use]
    pub fn taps(&self) -> usize {
        if self.window == Window::Cubic {
            return 4;
        }
        let raw = (self.filter_size as f64 / self.factor).ceil();
        let n = if raw.is_finite() && raw >= 2.0 {
            raw as usize
        } else {
            2
        };
        let cap = MAX_TAPS.min(self.filter_size.saturating_mul(MAX_STRETCH));
        let n = n.max(2).min(cap.max(2));
        if n % 2 == 0 { n } else { n + 1 }
    }
}

/// Build a coefficient bank.
///
/// # Errors
/// [`Error::InvalidData`] for a degenerate parameter set, or
/// [`Error::LimitExceeded`] if the bank would exceed the budget.
pub fn build_bank<T: Internal>(
    params: &DesignParams,
    budget: &mut Budget,
) -> Result<Bank<T>, Error> {
    if params.phases == 0 || params.filter_size == 0 {
        return Err(Error::InvalidData("degenerate filter parameters"));
    }
    if params.filter_size > MAX_FILTER_SIZE {
        return Err(Error::LimitExceeded {
            limit: "resample filter_size",
            requested: params.filter_size as u64,
            cap: MAX_FILTER_SIZE as u64,
        });
    }
    let raw_taps = (params.filter_size as f64 / params.factor).ceil();
    let cap = MAX_TAPS.min(params.filter_size.saturating_mul(MAX_STRETCH));
    if !raw_taps.is_finite() || raw_taps > cap as f64 {
        return Err(Error::LimitExceeded {
            limit: "resample filter taps",
            requested: if raw_taps.is_finite() {
                raw_taps as u64
            } else {
                u64::MAX
            },
            cap: cap as u64,
        });
    }
    let taps = params.taps();
    let total = params
        .phases
        .checked_mul(taps)
        .ok_or(Error::InvalidData("coefficient bank overflows"))?;
    budget.consume_fuel((total as u64).saturating_mul(FUEL_PER_COEFFICIENT))?;
    let mut coeffs: Vec<T> = budget.alloc::<T>(total).map_err(Error::from)?;

    let centre = taps / 2 - 1;
    let half = (taps as f64) * 0.5;
    let p = params.phases as f64;
    let factor = params.factor;

    for phase in 0..params.phases {
        let frac = phase as f64 / p;
        for j in 0..taps {
            let t = j as f64 - centre as f64 - frac;
            let v = match params.window {
                Window::Cubic => catmull_rom(t),
                Window::Kaiser => kaiser(t / half, params.kaiser_beta) * factor * sinc(factor * t),
                Window::BlackmanNuttall => blackman_nuttall(t / half) * factor * sinc(factor * t),
            };
            if let Some(slot) = coeffs.get_mut(phase * taps + j) {
                *slot = T::from_f64(v);
            }
        }
    }

    // MEASURED: a single scale derived from phase 0, not a per-phase
    // normalisation. See the module docs.
    let phase0_sum: f64 = coeffs
        .get(..taps)
        .map_or(1.0, |row| row.iter().map(|v| v.to_f64()).sum());
    if phase0_sum.is_finite() && phase0_sum.abs() > 1e-12 {
        let k = T::from_f64(1.0 / phase0_sum);
        for v in &mut coeffs {
            *v = v.mul(k);
        }
    }

    Ok(Bank {
        phases: params.phases,
        taps,
        centre,
        coeffs,
    })
}
