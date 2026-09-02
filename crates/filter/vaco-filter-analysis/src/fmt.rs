//! Shared `lavfi.<filter>.<key>` value formatting.
//!
//! Three distinct formatting rules are in play across this crate's filters,
//! each measured against `ffmpeg 8.1` individually rather than assumed to
//! generalise from `freezedetect`'s rule:
//!
//! * [`fixed6`] — plain `%f` (six decimal digits, **not** trimmed):
//!   `psnr`'s `mse`/`psnr` values, `identity`, `msad`. Measured:
//!   `lavfi.identity.identity.Y` prints `"1.000000"` on a self-identical
//!   pair, not `"1"`.
//! * [`trimmed_time`] — `freezedetect`'s rule (six decimals, trailing zeros
//!   trimmed, then a bare trailing `.` trimmed): `blackdetect`'s
//!   `black_start`/`black_end`, which are timestamps in seconds. Measured:
//!   `lavfi.black_start` prints `"0"` at `t=0`, and at a frame rate chosen so
//!   the boundary lands off a clean tick, `lavfi.black_end` prints
//!   `"1.000001"` — the same rounding artefact `freezedetect`'s own
//!   irregular-timestamp test exists to catch, reproduced here by reusing
//!   the identical `%.6f`-then-trim algorithm rather than a `blackdetect`-
//!   specific approximation of it.
//! * [`g6`] — C's `%g` with the default precision of six significant
//!   digits: `signalstats`'s every numeric field. Measured:
//!   `lavfi.signalstats.YAVG` prints `"61.5234"` (6 significant digits) and
//!   `"49.5"` (trailing zeros of the 6-significant-digit expansion trimmed),
//!   never `"61.523438"` (`%f`) or `"49.500000"`.
//!
//! `psnr`'s `inf` case for a perfect match is not a formatting rule at all —
//! it is `f64::INFINITY` printed as the literal string `"inf"`, handled at
//! each call site rather than folded into [`fixed6`], since only some of
//! this crate's values can ever be infinite.

/// `%f`: six decimal digits, never trimmed.
///
/// Measured against `ffmpeg 8.1`: `psnr`'s `mse.<c>`/`psnr.<c>`/`mse_avg`/
/// `psnr_avg` (except the `inf` case, handled separately), `identity`'s and
/// `msad`'s per-component and averaged values, all print exactly six
/// decimals regardless of trailing zeros.
#[must_use]
pub(crate) fn fixed6(value: f64) -> String {
    format!("{value:.6}")
}

/// The reference's "seconds since stream start" rule, identical to
/// `vaco-filter-temporal::freezedetect`'s private `format_lavfi_time`:
/// `%.6f`, then trailing zeros trimmed, then a bare trailing `.` trimmed.
///
/// Not shared as a cross-crate dependency — `freezedetect`'s copy is
/// `fn`-private to that crate and the algorithm is ten lines, so duplicating
/// it here costs less than inventing a dependency between two otherwise
/// unrelated filter crates for one helper (matching the precedent
/// `vaco-filter-temporal::video`'s own doc comment sets for `PlaneBuf`).
#[must_use]
pub(crate) fn trimmed_time(value: f64) -> String {
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// C's `printf("%g", value)` with the default precision of six significant
/// digits, restricted to the domain this crate's callers actually produce:
/// finite, non-negative, and small enough (`< 1e6`) that `%g`'s `%f` branch
/// applies rather than its `%e` one.
///
/// # Why not the general case
///
/// `%g` switches to exponential notation when the decimal exponent falls
/// outside `[-4, precision)`. Every `signalstats` field this crate computes
/// is a pixel sample statistic (`0..=255`), a saturation (`0..~181`) or a hue
/// (`0..=360`) — the exponent is always `0`, `1` or `2`, deep inside the
/// `%f` branch — so the exponential branch is not implemented. A future
/// caller feeding this a value outside that range would get a wrong answer
/// silently, which is why this is `pub(crate)` rather than exported: nothing
/// outside this crate's own measured domain should be trusted to it.
#[must_use]
pub(crate) fn g6(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let exponent = value.abs().log10().floor() as i32;
    let decimals = (5 - exponent).clamp(0, 6);
    #[allow(
        clippy::cast_sign_loss,
        reason = "decimals is clamped to 0..=6 immediately above"
    )]
    let mut s = format!("{value:.*}", decimals as usize);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Sample-count-weighted average: `sum(value * weight) / sum(weight)`.
///
/// `psnr`'s `mse_avg` and `ssim`'s `All` are measured to average their
/// per-component values weighted by how many samples that component has
/// (so a 4:2:0 U/V plane, at a quarter of luma's sample count, counts for a
/// quarter as much) — **not** a plain mean of the three numbers. See
/// `docs/filter/vaco-filter-analysis.md` for the measurement that
/// distinguishes this from [`simple_average`], which `identity`/`msad` use
/// instead.
#[must_use]
pub(crate) fn weighted_average(values: &[(f64, u64)]) -> f64 {
    let total_weight: u64 = values.iter().map(|(_, w)| *w).sum();
    if total_weight == 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "sample counts here are frame-sized, far below 2^53"
    )]
    let weighted_sum: f64 = values.iter().map(|(v, w)| v * (*w as f64)).sum();
    #[allow(clippy::cast_precision_loss, reason = "see above")]
    let denom = total_weight as f64;
    weighted_sum / denom
}

/// Plain, unweighted mean of the per-component values.
///
/// `identity_avg`/`msad_avg` are measured to average this way — every
/// component counts equally regardless of its plane's sample count — which
/// is the opposite of [`weighted_average`]. Confirmed by feeding `psnr`,
/// `identity` and `msad` the *same* asymmetric yuv420p input (luma differs,
/// chroma is identical) side by side: `psnr`'s `mse_avg` matches the
/// sample-weighted formula exactly and `identity_avg`/`msad_avg` match the
/// plain mean exactly, and neither formula fits the other filter's numbers.
#[must_use]
pub(crate) fn simple_average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss, reason = "component counts are 1..=4")]
    let n = values.len() as f64;
    values.iter().sum::<f64>() / n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed6_never_trims() {
        assert_eq!(fixed6(1.0), "1.000000");
        assert_eq!(fixed6(0.070_588_235_294_1), "0.070588");
    }

    #[test]
    fn trimmed_time_matches_freezedetects_measured_rule() {
        assert_eq!(trimmed_time(0.0), "0");
        assert_eq!(trimmed_time(1.0), "1");
        assert_eq!(trimmed_time(1.000_001), "1.000001");
        assert_eq!(trimmed_time(1.001), "1.001");
    }

    /// The four measured `signalstats` data points from this crate's docs:
    /// an exact half-integer, a repeating-decimal average, one with a
    /// trailing zero inside its 6-significant-digit expansion, and a bare
    /// integer.
    #[test]
    fn g6_matches_measured_signalstats_output() {
        assert_eq!(g6(49.5), "49.5");
        assert_eq!(g6(61.523_437_5), "61.5234");
        assert_eq!(g6(43.501_953_125), "43.502");
        assert_eq!(g6(22.0), "22");
        assert_eq!(g6(0.0), "0");
    }

    #[test]
    fn weighted_average_matches_measured_psnr_mse_avg() {
        // Measured: yuv420p 16x16, luma mse=32512.5 over 256 samples,
        // chroma mse=0 over 64 samples each -> mse_avg = 21675.0 exactly,
        // not 32512.5/3 (the plain-mean answer, which would be 10837.5).
        let avg = weighted_average(&[(32512.5, 256), (0.0, 64), (0.0, 64)]);
        assert!((avg - 21675.0).abs() < 1e-9);
    }

    #[test]
    fn simple_average_matches_measured_identity_avg() {
        // Same input as above, but identity/msad average unweighted:
        // measured identity_avg = 0.833333 = mean(0.5, 1.0, 1.0), not the
        // sample-weighted answer (which would be (0.5*256+64+64)/384).
        let avg = simple_average(&[0.5, 1.0, 1.0]);
        assert!((avg - 0.833_333_333_333).abs() < 1e-9);
    }
}
