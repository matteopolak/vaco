//! `signalstats` — per-frame pixel-value statistics.
//!
//! `ffmpeg -h filter=signalstats`: one video pad in, one out, several `stat`/
//! `out`/`color` options for the highlighting features this crate does not
//! implement (see below).
//!
//! # Scope, stated honestly
//!
//! The reference exports 29 keys (`man ffmpeg-filters`'s `signalstats`
//! entry, quoted here as the documented interface fact D7 allows): five
//! numbers per plane (`MIN`, `LOW`, `AVG`, `HIGH`, `MAX`) for `Y`, `U`, `V`,
//! five more for saturation and two for hue, three temporal `DIF` fields and
//! three `BITDEPTH` fields. This module implements the fifteen `MIN`/`LOW`/
//! `AVG`/`HIGH`/`MAX` fields across `Y`/`U`/`V` — the ones the brief calls
//! out as hand-computable and the ones every other measurement filter in
//! this crate would want to cross-check against — and **not**:
//!
//! * `SAT*`/`HUE*` — need an RGB<->HSV-style saturation/hue definition over
//!   YUV samples that was not pinned down in the time available; rather than
//!   guess at a formula and risk exactly the "false confirmation" this
//!   crate's brief warns about, it is left out.
//! * `*DIF` — temporal (needs the previous frame held as state); in scope
//!   for a future extension of this filter, not implemented now.
//! * `*BITDEPTH` — measured to be `1` for a perfectly constant plane and `8`
//!   for a full-range gradient, which rules out the naive `ceil(log2(distinct
//!   values))` reading (that gives `0`, not `1`, for a constant plane) and
//!   was not pinned down further in the time available.
//!
//! # Percentile rule, measured against `ffmpeg 8.1`
//!
//! ```text
//! $ ffprobe -of json -show_frames -f lavfi \
//!     -i "color=black:s=10x10,format=yuv420p,geq=lum='(Y*10+X)':cb=128:cr=128,signalstats"
//! YMIN=0 YLOW=9 YAVG=49.5 YHIGH=89 YMAX=99
//! ```
//!
//! A 10x10 plane with every value `0..=99` exactly once. `YLOW=9`: the
//! *smallest* value `v` whose cumulative count (`# samples <= v`) is `>=
//! total*0.1` (`10`) — `cumulative(9) = 10` (values `0..=9`), satisfying
//! `>= 10` at the smallest `v` that does. `YHIGH=89`: smallest `v` with
//! cumulative count `>= total*0.9` (`90`) — `cumulative(89) = 90`. `YAVG`
//! is the plain mean (`4950/100 = 49.5`), formatted with [`crate::fmt::g6`]
//! (`"49.5"`, not `"49.500000"`).
//!
//! This rule is confirmed only at a total that divides evenly by 10; a
//! non-round total's rounding behaviour (floor vs ceiling of `total*0.1`)
//! is not separately measured and is applied here as `>= (total as
//! f64 * 0.1).ceil() as u64`... actually implemented as a direct real
//! comparison against `total_samples as f64 * 0.1`, which agrees with the
//! measured integer case and is the natural generalisation.
//!
//! # Distinguishing input built for this filter
//!
//! The brief asks for "YMIN/YMAX/YAVG hand-computable"; a plane holding
//! every value `0..=99` exactly once distinguishes a percentile-based
//! `YLOW`/`YHIGH` from the naive alternative "10th/90th value after
//! sorting" (which would give `9`/`89` too, coincidentally, for a uniform
//! distribution — so a second check with a *skewed* distribution, several
//! values repeated near the extremes, is included below to catch a
//! sorted-index off-by-one that this uniform case cannot).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, PlaneRef};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::g6;
use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "signalstats",
    description: "Generate statistics from video analysis.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `(min, low_10pct, avg, high_90pct, max)` over one 8-bit plane's samples,
/// via a 256-bucket histogram (exact, no approximation).
fn plane_stats(plane: PlaneRef<'_>) -> Option<(u8, u8, f64, u8, u8)> {
    let mut histogram = [0u64; 256];
    let mut total: u64 = 0;
    let mut sum: u64 = 0;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for &sample in row {
            if let Some(slot) = histogram.get_mut(usize::from(sample)) {
                *slot += 1;
            }
            total += 1;
            sum += u64::from(sample);
        }
    }
    if total == 0 {
        return None;
    }
    let min = histogram.iter().position(|&c| c > 0)?;
    let max = histogram.iter().rposition(|&c| c > 0)?;
    #[allow(clippy::cast_precision_loss, reason = "sample counts are frame-sized")]
    let avg = sum as f64 / total as f64;
    #[allow(clippy::cast_precision_loss, reason = "sample counts are frame-sized")]
    let total_f = total as f64;
    let low_threshold = total_f * 0.1;
    let high_threshold = total_f * 0.9;
    let mut cumulative: u64 = 0;
    let mut low = min;
    let mut high = max;
    let mut low_found = false;
    for (value, &count) in histogram.iter().enumerate() {
        cumulative += count;
        #[allow(clippy::cast_precision_loss, reason = "cumulative is frame-sized")]
        let cumulative_f = cumulative as f64;
        if !low_found && cumulative_f >= low_threshold {
            low = value;
            low_found = true;
        }
        if cumulative_f >= high_threshold {
            high = value;
            break;
        }
    }
    #[allow(clippy::cast_possible_truncation, reason = "histogram indices are 0..=255")]
    Some((min as u8, low as u8, avg, high as u8, max as u8))
}

fn plane_tags(prefix: &str, plane: PlaneRef<'_>, out: &mut Vec<(String, String)>) {
    let Some((min, low, avg, high, max)) = plane_stats(plane) else {
        return;
    };
    out.push((format!("lavfi.signalstats.{prefix}MIN"), g6(f64::from(min))));
    out.push((format!("lavfi.signalstats.{prefix}LOW"), g6(f64::from(low))));
    out.push((format!("lavfi.signalstats.{prefix}AVG"), g6(avg)));
    out.push((format!("lavfi.signalstats.{prefix}HIGH"), g6(f64::from(high))));
    out.push((format!("lavfi.signalstats.{prefix}MAX"), g6(f64::from(max))));
}

#[derive(Debug, Default)]
pub(crate) struct SignalStats;

impl FrameFilter for SignalStats {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(measure(frame)))
    }
}

/// The actual measurement, factored out of [`FrameFilter::filter_frame`] so
/// it is unit-testable without a [`FilterContext`] — this filter never
/// touches `ctx`, matching `vaco-filter-temporal::freezedetect`'s precedent.
fn measure(mut frame: Frame) -> Frame {
    let mut tags = Vec::new();
    let plane_names = ["Y", "U", "V"];
    for (idx, name) in plane_names.iter().enumerate() {
        let Some(plane) = frame.plane(idx) else { break };
        plane_tags(name, plane, &mut tags);
    }
    for (key, value) in tags {
        frame.set_metadata(key, value);
    }
    frame
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(SignalStats)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn ramp_frame_yuv(w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            let mut v: u16 = 0;
            for y in 0..p.rows() {
                if let Some(row) = p.row_mut(y) {
                    for byte in row {
                        #[allow(clippy::cast_possible_truncation, reason = "v stays < 256 by construction")]
                        {
                            *byte = v as u8;
                        }
                        v += 1;
                    }
                }
            }
        }
        if let Some(mut p) = f.plane_mut(1) {
            p.fill(128);
        }
        if let Some(mut p) = f.plane_mut(2) {
            p.fill(128);
        }
        f
    }

    /// Hand-computable oracle: a 10x10 plane holding every value `0..=99`
    /// exactly once. `YMIN=0`, `YMAX=99`, `YAVG=4950/100=49.5` are
    /// arithmetic identities of that construction, independent of this
    /// implementation. `YLOW`/`YHIGH` (10th/90th percentile by cumulative
    /// count) are `9`/`89`, matched against `ffmpeg 8.1` exactly.
    #[test]
    fn uniform_ramp_matches_hand_computed_stats() {
        let f = ramp_frame_yuv(10, 10);
        let plane = f.plane(0).unwrap();
        let (min, low, avg, high, max) = plane_stats(plane).unwrap();
        assert_eq!(min, 0);
        assert_eq!(max, 99);
        assert!((avg - 49.5).abs() < 1e-12);
        assert_eq!(low, 9);
        assert_eq!(high, 89);
    }

    /// Distinguishing input: a skewed distribution (most samples clustered
    /// at the low end, a few at the very top) rules out "10th/90th value
    /// after sorting the *distinct* values" as an alternative hypothesis —
    /// on the uniform ramp above, sorted-distinct-value indexing and
    /// cumulative-count percentile coincide by construction (every value
    /// appears exactly once), but they diverge here.
    #[test]
    fn skewed_distribution_uses_cumulative_count_not_sorted_index() {
        let pool = FramePool::default();
        // 90 samples at value 0, 9 samples at value 1, 1 sample at value 99.
        let mut f = pool.acquire_video(PixFmt::Gray8, 10, 10).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..10 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(0);
                }
            }
            // Overwrite 9 samples to value 1 and 1 sample to value 99.
            if let Some(row) = p.row_mut(9) {
                for (i, byte) in row.iter_mut().enumerate() {
                    *byte = if i < 9 { 1 } else { 99 };
                }
            }
        }
        let (min, low, _avg, high, max) = plane_stats(f.plane(0).unwrap()).unwrap();
        assert_eq!(min, 0);
        assert_eq!(max, 99);
        // Cumulative-count percentile: 90 of the 100 samples already sit at
        // value 0, so the cumulative count clears *both* the 10% and the
        // 90% threshold at value 0 itself — YLOW=YHIGH=0. A "10th/90th
        // distinct value after sorting the unique values" hypothesis would
        // instead index into `[0, 1, 99]` (3 distinct values) and land on a
        // different pair (e.g. `0`/`99`, treating the 90th percentile as
        // "near the top of the distinct-value list"). The two hypotheses
        // agree on the uniform ramp above and disagree here, which is what
        // makes this the distinguishing input.
        assert_eq!(low, 0);
        assert_eq!(high, 0);
    }

    #[test]
    fn output_carries_no_v_key_when_the_frame_has_no_third_plane() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(16);
        }
        let out = measure(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.YMIN"), Some("16"));
        assert!(out.metadata_get("lavfi.signalstats.UMIN").is_none());
    }
}
