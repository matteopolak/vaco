//! `entropy` — graylevel histogram entropy per plane.
//!
//! One video pad in, one out. One option, `mode` (`normal`/`diff`, default
//! `normal`).
//!
//! # Formula, measured against `ffmpeg 8.1`
//!
//! ```text
//! lavfi.entropy.entropy.normal.Y="8.000000" lavfi.entropy.normalized_entropy.normal.Y="1.000000"
//! ```
//!
//! `mode=normal`: plain Shannon entropy of the 256-bucket sample histogram
//! as a probability distribution — `p_i = count_i/total`,
//! `entropy = -sum(p_i*log2(p_i))` for `p_i>0`. A 16x16 plane holding every
//! value `0..=255` exactly once gives every `p_i=1/256`, so
//! `entropy=log2(256)=8` exactly. `normalized_entropy` is `entropy/8.0`
//! (`log2(256)`, the max possible for an 8-bit histogram) — confirmed on a
//! flat plane (`0/8=0`), a 50/50 split (`1/8=0.125`), and a skewed
//! three-level histogram (`0.515895/8=0.064487`), which also rules out
//! dividing by `log2(distinct values present)` instead (`log2(3)=1.585`
//! would give `0.3255`, not the measured `0.064487`).
//!
//! `mode=diff` uses `delta_i = |hist_i - hist_(i-1)|` for `i` in `1..256` in
//! place of `hist_i`, still normalised by the same `total` sample count, not
//! `sum(delta)` — confirmed on a skewed histogram (`90`/`9`/`1` at values
//! `0`/`1`/`99`) where the two normalisations disagree: `sum(delta)=92`
//! would give `0.631636`, `total=100` (the measured rule) gives `0.691776`,
//! matching the reference.
//!
//! A flat plane alone can't separate `/total` from `/sum(delta)` (both
//! degenerate to the same case) — the skewed fixture above is what does.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, PlaneRef};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::fixed6;
use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "entropy",
    description: "Measure video frames entropy.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Diff,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Diff => "diff",
        }
    }
}

/// The maximum possible entropy of a 256-bucket (8-bit) histogram,
/// `log2(256)`. [`crate::fmt`]'s `normalized_entropy` fields divide by this
/// constant regardless of how many distinct values the plane actually uses —
/// see this module's doc for the measurement that rules out the alternative
/// (dividing by the entropy of the distinct-value count actually present).
const MAX_ENTROPY: f64 = 8.0;

fn histogram(plane: PlaneRef<'_>) -> ([u64; 256], u64) {
    let mut hist = [0u64; 256];
    let mut total: u64 = 0;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for &sample in row {
            if let Some(slot) = hist.get_mut(usize::from(sample)) {
                *slot += 1;
            }
            total += 1;
        }
    }
    (hist, total)
}

/// `-sum(p_i * log2(p_i))` for `p_i = counts[i] / total`, `p_i > 0`.
fn shannon(counts: &[u64], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "sample/delta counts are frame-sized"
    )]
    let total_f = total as f64;
    let sum: f64 = counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            #[allow(clippy::cast_precision_loss, reason = "counts are frame-sized")]
            let p = c as f64 / total_f;
            -p * p.log2()
        })
        .sum();
    // A single-bucket (zero-entropy) distribution computes `-1.0 *
    // 1.0f64.log2()` = `-1.0 * 0.0` = negative zero, which prints as
    // `"-0.000000"` — not, as far as this crate could measure, the
    // reference's own spelling of "no entropy". Normalised to positive
    // zero rather than shipping a value that differs only in sign bit.
    if sum == 0.0 { 0.0 } else { sum }
}

/// `(entropy, normalized_entropy)` for one plane under `mode`.
fn plane_entropy(plane: PlaneRef<'_>, mode: Mode) -> Option<(f64, f64)> {
    let (hist, total) = histogram(plane);
    if total == 0 {
        return None;
    }
    let entropy = match mode {
        Mode::Normal => shannon(&hist, total),
        Mode::Diff => {
            let mut delta = [0u64; 256];
            for i in 1..256 {
                let (Some(&cur), Some(&prev), Some(slot)) =
                    (hist.get(i), hist.get(i - 1), delta.get_mut(i))
                else {
                    continue;
                };
                *slot = cur.abs_diff(prev);
            }
            shannon(&delta, total)
        }
    };
    Some((entropy, entropy / MAX_ENTROPY))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    mode: Mode,
}

impl Default for Options {
    fn default() -> Self {
        Self { mode: Mode::Normal }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
}

impl Filter {
    pub(crate) const fn new(opts: Options) -> Self {
        Self { opts }
    }

    fn step(&mut self, mut frame: Frame) -> Frame {
        let plane_names = ["Y", "U", "V"];
        let mut tags = Vec::new();
        for (idx, name) in plane_names.iter().enumerate() {
            let Some(plane) = frame.plane(idx) else { break };
            let Some((entropy, normalized)) = plane_entropy(plane, self.opts.mode) else {
                continue;
            };
            let mode = self.opts.mode.label();
            tags.push((
                format!("lavfi.entropy.entropy.{mode}.{name}"),
                fixed6(entropy),
            ));
            tags.push((
                format!("lavfi.entropy.normalized_entropy.{mode}.{name}"),
                fixed6(normalized),
            ));
        }
        for (key, value) in tags {
            frame.set_metadata(key, value);
        }
        frame
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let mode = match req.named("mode").as_deref() {
        Some("diff") => Mode::Diff,
        _ => Mode::Normal,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options { mode }))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn ramp_frame(w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            let mut v: u16 = 0;
            for y in 0..p.rows() {
                if let Some(row) = p.row_mut(y) {
                    for byte in row {
                        #[allow(clippy::cast_possible_truncation, reason = "v stays < 256")]
                        {
                            *byte = v as u8;
                        }
                        v += 1;
                    }
                }
            }
        }
        f
    }

    fn flat_frame(value: u8, w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    /// Independent oracle: a 16x16 plane holding every one of the 256
    /// possible 8-bit values exactly once is, by construction, a uniform
    /// distribution over 256 outcomes — its Shannon entropy is `log2(256) =
    /// 8` as an algebraic identity, not a property of this implementation.
    #[test]
    fn uniform_all_values_once_scores_the_maximum() {
        let f = ramp_frame(16, 16);
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert_eq!(
            out.metadata_get("lavfi.entropy.entropy.normal.Y"),
            Some("8.000000")
        );
        assert_eq!(
            out.metadata_get("lavfi.entropy.normalized_entropy.normal.Y"),
            Some("1.000000")
        );
    }

    /// A flat plane has one histogram bucket holding every sample: entropy
    /// of a one-outcome distribution is 0 by definition (`log2(1) = 0`).
    #[test]
    fn flat_plane_is_zero_entropy() {
        let f = flat_frame(128, 8, 8);
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert_eq!(
            out.metadata_get("lavfi.entropy.entropy.normal.Y"),
            Some("0.000000")
        );
    }

    /// Distinguishing input: a skewed three-level histogram (90/9/1 at
    /// values 0/1/99, mirroring `signalstats`'s own skew fixture) rules out
    /// two different alternative hypotheses at once. In `normal` mode it
    /// pins `normalized_entropy` to `entropy/8` rather than
    /// `entropy/log2(distinct_values)` (which would give a very different
    /// number here, `log2(3)=1.585`, since only 3 of 256 buckets are
    /// nonzero). Measured against `ffmpeg 8.1`.
    #[test]
    fn skewed_histogram_normalizes_by_eight_not_by_distinct_count() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 10, 10).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..9 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(0);
                }
            }
            if let Some(row) = p.row_mut(9) {
                for (i, byte) in row.iter_mut().enumerate() {
                    *byte = if i < 9 { 1 } else { 99 };
                }
            }
        }
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert_eq!(
            out.metadata_get("lavfi.entropy.entropy.normal.Y"),
            Some("0.515895")
        );
        assert_eq!(
            out.metadata_get("lavfi.entropy.normalized_entropy.normal.Y"),
            Some("0.064487")
        );
    }

    /// Distinguishing input for `mode=diff`: the same skewed histogram as
    /// above, which makes `sum(delta)=92` diverge from `total=100` — the two
    /// candidate normalisations for `diff` mode's histogram-of-deltas step.
    /// Measured against `ffmpeg 8.1`: the reference matches normalising by
    /// `total` (`0.691776`), not `sum(delta)` (which would give `0.631636`).
    #[test]
    fn diff_mode_normalizes_deltas_by_total_not_by_their_own_sum() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 10, 10).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..9 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(0);
                }
            }
            if let Some(row) = p.row_mut(9) {
                for (i, byte) in row.iter_mut().enumerate() {
                    *byte = if i < 9 { 1 } else { 99 };
                }
            }
        }
        let mut filt = Filter::new(Options::default());
        filt.opts.mode = Mode::Diff;
        let out = filt.step(f);
        assert_eq!(
            out.metadata_get("lavfi.entropy.entropy.diff.Y"),
            Some("0.691776")
        );
        assert_eq!(
            out.metadata_get("lavfi.entropy.normalized_entropy.diff.Y"),
            Some("0.086472")
        );
    }

    /// `mode=diff` on a uniform-every-value-once plane: every histogram
    /// bucket holds exactly 1, so every delta is 0 (`|1-1|=0`) except at the
    /// very first computed delta (`i=1`, no wraparound), and even that one
    /// compares two buckets that both hold 1 — so this fixture must score
    /// exactly 0, an algebraic identity independent of how `diff` mode is
    /// wired, unlike `normal` mode's 8.0 on the same input (proving `mode`
    /// actually changes behaviour rather than being ignored).
    #[test]
    fn diff_mode_on_a_flat_histogram_is_zero() {
        let f = ramp_frame(16, 16);
        let mut filt = Filter::new(Options { mode: Mode::Diff });
        let out = filt.step(f);
        assert_eq!(
            out.metadata_get("lavfi.entropy.entropy.diff.Y"),
            Some("0.000000")
        );
    }
}
