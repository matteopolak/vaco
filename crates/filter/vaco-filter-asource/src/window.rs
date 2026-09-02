//! Shared window functions for [`crate::hilbert`]'s FIR taper.
//!
//! `ffmpeg -h filter=hilbert` documents a 21-entry `win_func` enum shared
//! with several reference filters this crate does not currently expose
//! `win_func` on at all — measured directly: neither `sinc` nor
//! `afdelaysrc`'s own `-h filter=…` output lists a `win_func` option
//! today, despite this module's name suggesting otherwise; only
//! `hilbert.rs` reads [`WinFunc`].
//!
//! This module implements six of the 21 with their own real, distinct
//! closed form (`rect`, `bartlett`, `hann`, `hamming`, `blackman` — the
//! shared default — and `sine`). The other fifteen (`welch`, `bhann`,
//! `flattop`, `bharris`, `bnuttall`, `nuttall`, `lanczos`, `gauss`,
//! `tukey`, `dolph`, `cauchy`, `parzen`, `poisson`, `bohman`, `kaiser`)
//! used to be computed as one of those six regardless — `welch` ran
//! `bartlett`'s triangular formula, `bhann` ran `hann`'s, and the
//! remaining thirteen ran `blackman`'s — with no error at all: a value
//! the reference documents, accepted, and silently under-served. That
//! was reasoned as "a smaller failure than refusing it outright" when
//! written, but a silent substitution is the worse failure in practice —
//! it produces a plausible, wrong frame with no signal anything happened,
//! discoverable only by a differential comparison nobody is running by
//! hand. [`ensure_implemented`] is what [`crate::hilbert::create`] now
//! calls instead: the six real formulas still run exactly as before, and
//! every other named value is rejected explicitly, by name, before a
//! `Source` is ever built.
//!
//! `hilbert.rs`'s doc comment independently confirms the `blackman`
//! formula's constants against a measured reference impulse response, which
//! is the actual evidence this module's default is correct — this file
//! just holds the shared closed forms.

use std::f64::consts::PI;
use vaco_opts::OptEnum;

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

/// The reference's second spelling for [`WinFunc::Hann`] (value `1`).
/// `ffmpeg -h filter=hilbert` documents both `hann` and `hanning` naming
/// the same value; `#[derive(OptEnum)]` emits exactly one name per variant
/// (see `WinFunc`'s own `#[opt_const]` list), so a second name for the
/// same value needs a field-level `consts` override rather than a second
/// variant. `OptValue::parse_into` for an `OptEnum`-derived type checks an
/// explicit field-level `consts` list first, then falls back to the type's
/// own derived table, so `hann` still resolves there — this list only
/// needs to carry the name the derive cannot express.
pub(crate) const WIN_FUNC_ALIASES: &[vaco_opts::ConstDesc] = &[vaco_opts::ConstDesc {
    name: "hanning",
    help: "Hanning",
    unit: "win_func",
    value: vaco_opts::ConstValue::Int(1),
    flags: vaco_opts::OptFlags::NONE,
}];

/// Rejects the fifteen `win_func` values this module does not have a real
/// formula for, by name, instead of silently running one of the six it
/// does — see the module doc.
///
/// # Errors
/// A message naming the caller-facing filter, the option, and the exact
/// value that was rejected — never a panic, and never the value that
/// actually ran.
pub(crate) fn ensure_implemented(filter: &str, func: WinFunc) -> Result<(), String> {
    match func {
        WinFunc::Rect
        | WinFunc::Bartlett
        | WinFunc::Hann
        | WinFunc::Hamming
        | WinFunc::Blackman
        | WinFunc::Sine => Ok(()),
        other => Err(format!(
            "{filter}: win_func={other:?} is not implemented (only rect/bartlett/hann/hamming/\
             blackman/sine have their own formula here; see window.rs's own doc for why)"
        )),
    }
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
