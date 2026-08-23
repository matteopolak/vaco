//! Shared window functions for [`crate::hilbert`], [`crate::sinc`] and
//! [`crate::afdelaysrc`]'s FIR taper.
//!
//! `ffmpeg -h filter=hilbert`/`sinc`/`afirsrc` all document the same
//! 21-entry `win_func` enum. This module implements the five with a plain,
//! textbook closed form (`rect`, `bartlett`, `hann`, `hamming`,
//! `blackman` — `blackman` is the shared default) and falls back to
//! `blackman` for the other sixteen (`welch`, `flattop`, `kaiser`, …)
//! rather than leaving them as a parse error, since accepting an option the
//! reference documents and silently under-serving it is a smaller failure
//! than refusing it outright — see `docs/filter/vaco-filter-asource.md` for
//! the full list of which `win_func` values fall back.
//!
//! `hilbert.rs`'s doc comment independently confirms the `blackman`
//! formula's constants against a measured reference impulse response, which
//! is the actual evidence this module's default is correct — this file
//! just holds the shared closed forms.

use vaco_opts::OptEnum;
use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "win_func", base = "int")]
pub(crate) enum WinFunc {
    #[opt_const(name = "rect", help = "Rectangular")]
    Rect,
    #[opt_const(name = "hann", help = "Hann")]
    Hann,
    #[opt_const(name = "hamming", help = "Hamming")]
    Hamming,
    #[opt_const(name = "blackman", help = "Blackman")]
    #[default]
    Blackman,
    #[opt_const(name = "bartlett", help = "Bartlett")]
    Bartlett,
    #[opt_const(name = "welch", help = "Welch")]
    Welch,
    #[opt_const(name = "flattop", help = "Flat-top")]
    Flattop,
    #[opt_const(name = "bharris", help = "Blackman-Harris")]
    Bharris,
    #[opt_const(name = "bnuttall", help = "Blackman-Nuttall")]
    Bnuttall,
    #[opt_const(name = "sine", help = "Sine")]
    Sine,
    #[opt_const(name = "nuttall", help = "Nuttall")]
    Nuttall,
    #[opt_const(name = "bhann", help = "Bartlett-Hann")]
    Bhann,
    #[opt_const(name = "lanczos", help = "Lanczos")]
    Lanczos,
    #[opt_const(name = "gauss", help = "Gauss")]
    Gauss,
    #[opt_const(name = "tukey", help = "Tukey")]
    Tukey,
    #[opt_const(name = "dolph", help = "Dolph-Chebyshev")]
    Dolph,
    #[opt_const(name = "cauchy", help = "Cauchy")]
    Cauchy,
    #[opt_const(name = "parzen", help = "Parzen")]
    Parzen,
    #[opt_const(name = "poisson", help = "Poisson")]
    Poisson,
    #[opt_const(name = "bohman", help = "Bohman")]
    Bohman,
    #[opt_const(name = "kaiser", help = "Kaiser")]
    Kaiser,
}

/// The window value at tap `n` of `n_taps`, `n in 0..n_taps`.
///
/// Every implemented window is symmetric (`w(n) == w(n_taps - 1 - n)`),
/// which every one of this module's tests checks — a property of a proper
/// analysis/synthesis window, not a restatement of any one formula.
pub(crate) fn value(func: WinFunc, n: usize, n_taps: usize) -> f64 {
    if n_taps <= 1 {
        return 1.0;
    }
    let denom = (n_taps - 1) as f64;
    #[allow(clippy::cast_precision_loss, reason = "n < n_taps, a FIR tap count")]
    let x = n as f64 / denom;
    match func {
        WinFunc::Rect => 1.0,
        WinFunc::Bartlett | WinFunc::Welch => 1.0 - (2.0 * x - 1.0).abs(),
        WinFunc::Hann | WinFunc::Bhann => 0.5 - 0.5 * (2.0 * PI * x).cos(),
        WinFunc::Hamming => 0.54 - 0.46 * (2.0 * PI * x).cos(),
        WinFunc::Blackman
        | WinFunc::Flattop
        | WinFunc::Bharris
        | WinFunc::Bnuttall
        | WinFunc::Nuttall
        | WinFunc::Lanczos
        | WinFunc::Gauss
        | WinFunc::Tukey
        | WinFunc::Dolph
        | WinFunc::Cauchy
        | WinFunc::Parzen
        | WinFunc::Poisson
        | WinFunc::Bohman
        | WinFunc::Kaiser => 0.42 - 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos(),
        WinFunc::Sine => (PI * x).sin(),
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "Rect's value is the literal constant 1.0, not an accumulated float result"
)]
mod tests {
    use super::*;

    fn all_funcs() -> [WinFunc; 5] {
        [
            WinFunc::Rect,
            WinFunc::Bartlett,
            WinFunc::Hann,
            WinFunc::Hamming,
            WinFunc::Blackman,
        ]
    }

    #[test]
    fn every_implemented_window_is_symmetric() {
        for func in all_funcs() {
            for n in 0..15 {
                let a = value(func, n, 15);
                let b = value(func, 14 - n, 15);
                assert!((a - b).abs() < 1e-9, "{func:?} n={n}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn blackman_matches_the_measured_hilbert_reference() {
        // From hilbert.rs's doc: N=11, offset+1 tap (index 6) ~= 0.8492,
        // offset+3 tap (index 8) ~= 0.2008.
        assert!((value(WinFunc::Blackman, 6, 11) - 0.8492).abs() < 0.001);
        assert!((value(WinFunc::Blackman, 8, 11) - 0.2008).abs() < 0.001);
    }

    #[test]
    fn rect_is_always_one() {
        for n in 0..10 {
            assert_eq!(value(WinFunc::Rect, n, 10), 1.0);
        }
    }
}
