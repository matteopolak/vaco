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
//!
//! # 2026-08-23 addition: `SAT*`, `HUE*`, `*DIF`, `*BITDEPTH`
//!
//! Fourteen more keys, measured against `ffmpeg 8.1` rather than guessed:
//!
//! * `SAT{MIN,LOW,AVG,HIGH,MAX}` — the same percentile machinery as
//!   `Y{MIN,LOW,AVG,HIGH,MAX}` above, over a per-pixel saturation value
//!   `floor(sqrt((U-128)^2 + (V-128)^2))`. Confirmed on three solid colours
//!   (`color=red`/`blue`/`green` under `format=yuv420p`): red (`U=90,
//!   V=240`) measures `SATMAX=118`, matching `floor(sqrt(38^2+112^2)) =
//!   floor(118.27) = 118` exactly; green (`U=91,V=81`) measures `SATMAX=59`,
//!   matching `floor(sqrt(37^2+47^2)) = floor(59.82) = 59` — the green
//!   fixture is what pins **floor**, not round (`round(59.82)` would be
//!   `60`, and red/blue's own values happen to floor and round identically).
//! * `HUE{MED,AVG}` — a per-pixel hue value
//!   `floor((atan2(U-128, V-128) in degrees + 180) mod 360)`, degrees.
//!   Confirmed on the same three solid colours: red measures `161`, matching
//!   `floor((atan2(-38,112)*180/pi + 180) mod 360) = floor(161.259) = 161`;
//!   blue and green pin the `atan2(u_dev, v_dev)` argument order and the
//!   `+180` offset (both `atan2(v_dev,u_dev)` and no offset were checked and
//!   ruled out — neither matches any of the three colours). **`HUEMED`
//!   equals `HUEAVG` on every fixture measured here** (all three are
//!   perfectly flat colour fields, where every formulation of "average" and
//!   "median" coincide); this crate computes `HUEAVG` as the plain mean of
//!   the per-pixel hue values and `HUEMED` as the same cumulative-count
//!   50th-percentile rule `YLOW`/`YHIGH` use at 10%/90%, which is a
//!   reasonable generalisation but is **not independently confirmed** on any
//!   fixture where the two would actually differ — no such measurement was
//!   made in the time available.
//! * `{Y,U,V}DIF` — mean absolute per-sample difference against the
//!   *previous* frame (`0` on the first frame, or after any frame whose
//!   plane geometry does not match — there is nothing to compare against).
//!   Confirmed: a two-frame sequence where exactly half of `Y`'s samples
//!   change by exactly `20` between frames measures `YDIF=10` — the mean of
//!   `20` over half the samples and `0` over the other half — ruling out a
//!   "mean over only the changed samples" alternative (which would give
//!   `20`). Formatted with [`crate::fmt::g6`], like every other field in
//!   this module.
//! * `{Y,U,V}BITDEPTH` — `popcount` of the bitwise OR of every distinct
//!   sample value present in the plane. Confirmed on a flat plane at value
//!   `100` (`0b110_0100`, three set bits) measuring `YBITDEPTH=3`, and on a
//!   two-level plane holding `100` and `120` (`0b110_0100 | 0b111_1000 =
//!   0b111_1100`, five set bits) measuring `YBITDEPTH=5` — the two-level
//!   fixture is what rules out "popcount of a single representative value"
//!   as a hypothesis (it would still give a wrong, single-value-dependent
//!   answer even by coincidence on the flat case). **Corrects a claim in
//!   this crate's own initial scoping**: `docs/filter/vaco-filter-analysis.md`
//!   originally reported "`BITDEPTH` measured `1` for a constant plane",
//!   generalising from a single fixture that happened to use the value
//!   `128` (`0b1000_0000`, exactly one set bit) — true for that value, not
//!   for "a constant plane" in general, as this crate's own `100`-valued
//!   flat-plane fixture now demonstrates. Formatted as a plain `to_string()`
//!   integer, not [`crate::fmt::g6`] (measured: `"3"`, not `"3.00000"`).
//!
//! **Still not implemented**: `TOUT`/`VREP`/`BRNG` (the `stat=`/`out=`
//! option's temporal-outlier / vertical-line-repetition / broadcast-range
//! *detection* features) — these are not part of the default metadata
//! export at all (`man ffmpeg-filters` documents them as values of the
//! `stat`/`out` options, not as keys `signalstats` emits unconditionally),
//! and implementing them means a per-pixel classification pass this crate
//! has not measured. Left for a follow-up, honestly, rather than guessed.

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

/// `floor(sqrt((u-128)^2 + (v-128)^2))` for one chroma sample pair — see
/// this module's doc for the measurement that pins `floor`, not `round`.
fn saturation(u: u8, v: u8) -> u8 {
    let du = f64::from(u) - 128.0;
    let dv = f64::from(v) - 128.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sqrt(du^2+dv^2) is bounded by sqrt(128^2*2) ~= 181, well within u8"
    )]
    {
        du.hypot(dv).floor() as u8
    }
}

/// `floor((atan2(u-128, v-128) in degrees + 180) mod 360)` — see this
/// module's doc for the three-colour measurement that pins the argument
/// order and the `+180` offset.
fn hue_degrees(u: u8, v: u8) -> u16 {
    let du = f64::from(u) - 128.0;
    let dv = f64::from(v) - 128.0;
    let degrees = du.atan2(dv).to_degrees() + 180.0;
    let wrapped = degrees.rem_euclid(360.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "wrapped is in 0.0..360.0 by rem_euclid"
    )]
    {
        wrapped.floor() as u16
    }
}

/// `(min, p10, avg, p90, max)` over an arbitrary `0..=max_bucket` histogram,
/// the same cumulative-count percentile rule [`plane_stats`] uses, factored
/// out so `SAT*` can share it instead of re-deriving the percentile loop.
fn histogram_stats(histogram: &[u64], total: u64) -> Option<(u16, u16, f64, u16, u16)> {
    if total == 0 {
        return None;
    }
    let min = histogram.iter().position(|&c| c > 0)?;
    let max = histogram.iter().rposition(|&c| c > 0)?;
    #[allow(clippy::cast_precision_loss, reason = "sample counts are frame-sized")]
    let total_f = total as f64;
    let low_threshold = total_f * 0.1;
    let high_threshold = total_f * 0.9;
    let mut cumulative: u64 = 0;
    let mut sum: u64 = 0;
    let mut low = min;
    let mut high = max;
    let mut low_found = false;
    for (value, &count) in histogram.iter().enumerate() {
        cumulative += count;
        sum += (value as u64).saturating_mul(count);
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
    #[allow(clippy::cast_precision_loss, reason = "sum/total are frame-sized")]
    let avg = sum as f64 / total_f;
    #[allow(clippy::cast_possible_truncation, reason = "histogram indices fit u16 (max 360)")]
    Some((min as u16, low as u16, avg, high as u16, max as u16))
}

/// `SAT{MIN,LOW,AVG,HIGH,MAX}` and `HUE{MED,AVG}`, from the `U`/`V` planes
/// together (chroma-resolution, not luma-resolution — matching the
/// reference's own per-chroma-sample computation).
fn sat_hue_tags(u_plane: PlaneRef<'_>, v_plane: PlaneRef<'_>, out: &mut Vec<(String, String)>) {
    let mut sat_hist = [0u64; 256];
    let mut hue_hist = [0u64; 360];
    let mut hue_sum: f64 = 0.0;
    let mut total: u64 = 0;
    let rows = u_plane.rows().min(v_plane.rows());
    for y in 0..rows {
        let (Some(ru), Some(rv)) = (u_plane.row(y), v_plane.row(y)) else {
            continue;
        };
        let width = ru.len().min(rv.len());
        for x in 0..width {
            let (Some(&u), Some(&v)) = (ru.get(x), rv.get(x)) else {
                continue;
            };
            if let Some(slot) = sat_hist.get_mut(usize::from(saturation(u, v))) {
                *slot += 1;
            }
            let hue = hue_degrees(u, v);
            if let Some(slot) = hue_hist.get_mut(usize::from(hue)) {
                *slot += 1;
            }
            hue_sum += f64::from(hue);
            total += 1;
        }
    }
    if total == 0 {
        return;
    }
    if let Some((min, low, avg, high, max)) = histogram_stats(&sat_hist, total) {
        out.push(("lavfi.signalstats.SATMIN".to_owned(), g6(f64::from(min))));
        out.push(("lavfi.signalstats.SATLOW".to_owned(), g6(f64::from(low))));
        out.push(("lavfi.signalstats.SATAVG".to_owned(), g6(avg)));
        out.push(("lavfi.signalstats.SATHIGH".to_owned(), g6(f64::from(high))));
        out.push(("lavfi.signalstats.SATMAX".to_owned(), g6(f64::from(max))));
    }
    // `HUEMED`: the same 50%-cumulative-count rule LOW/HIGH use at 10%/90%
    // — see this module's doc for why this is a generalisation, not an
    // independently confirmed rule.
    #[allow(clippy::cast_precision_loss, reason = "total is frame-sized")]
    let median_threshold = total as f64 * 0.5;
    let mut cumulative: u64 = 0;
    let mut median = 0u16;
    for (value, &count) in hue_hist.iter().enumerate() {
        cumulative += count;
        #[allow(clippy::cast_precision_loss, reason = "cumulative is frame-sized")]
        let cumulative_f = cumulative as f64;
        if cumulative_f >= median_threshold {
            #[allow(clippy::cast_possible_truncation, reason = "hue_hist has 360 entries")]
            {
                median = value as u16;
            }
            break;
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "total is frame-sized")]
    let hue_avg = hue_sum / total as f64;
    out.push(("lavfi.signalstats.HUEMED".to_owned(), g6(f64::from(median))));
    out.push(("lavfi.signalstats.HUEAVG".to_owned(), g6(hue_avg)));
}

/// Mean absolute per-sample difference between `cur` and `prev`, or `None`
/// if the planes' geometry does not match (nothing to compare).
fn dif(cur: PlaneRef<'_>, prev: &[Vec<u8>]) -> Option<f64> {
    let rows = cur.rows().min(prev.len());
    if rows == 0 {
        return None;
    }
    let mut sum: u64 = 0;
    let mut total: u64 = 0;
    for y in 0..rows {
        let Some(row) = cur.row(y) else { continue };
        let Some(prev_row) = prev.get(y) else { continue };
        let width = row.len().min(prev_row.len());
        for x in 0..width {
            let (Some(&cur), Some(&prev)) = (row.get(x), prev_row.get(x)) else {
                continue;
            };
            sum += u64::from(cur.abs_diff(prev));
            total += 1;
        }
    }
    if total == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss, reason = "sum/total are frame-sized")]
    Some(sum as f64 / total as f64)
}

/// `popcount` of the bitwise OR of every distinct sample value present —
/// see this module's doc for the two-level fixture that pins this formula
/// down against "popcount of a single representative value".
fn bit_depth(plane: PlaneRef<'_>) -> u32 {
    let mut union: u8 = 0;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for &sample in row {
            union |= sample;
        }
    }
    union.count_ones()
}

fn plane_bytes(plane: PlaneRef<'_>) -> Vec<Vec<u8>> {
    (0..plane.rows()).map(|y| plane.row(y).map(<[u8]>::to_vec).unwrap_or_default()).collect()
}

#[derive(Debug, Default)]
pub(crate) struct SignalStats {
    /// Previous frame's `Y`/`U`/`V` plane bytes, for `*DIF`. `None` before
    /// the first frame, or for a plane index the previous frame lacked.
    previous: [Option<Vec<Vec<u8>>>; 3],
}

impl FrameFilter for SignalStats {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }
}

impl SignalStats {
    /// The actual measurement. Not a free function like this crate's other
    /// filters, because `*DIF` needs the previous frame's samples held as
    /// state — `vaco-filter-temporal::tmix`'s ring-buffer precedent is the
    /// closest shape this crate already has for "a filter that needs more
    /// than the current frame".
    fn step(&mut self, mut frame: Frame) -> Frame {
        let mut tags = Vec::new();
        let plane_names = ["Y", "U", "V"];
        let mut current: [Option<Vec<Vec<u8>>>; 3] = [None, None, None];
        for (idx, name) in plane_names.iter().enumerate() {
            let Some(plane) = frame.plane(idx) else { break };
            plane_tags(name, plane, &mut tags);
            let bytes = plane_bytes(plane);
            let dif_value = self
                .previous
                .get(idx)
                .and_then(Option::as_ref)
                .and_then(|prev| dif(plane, prev))
                .unwrap_or(0.0);
            tags.push((format!("lavfi.signalstats.{name}DIF"), g6(dif_value)));
            tags.push((format!("lavfi.signalstats.{name}BITDEPTH"), bit_depth(plane).to_string()));
            if let Some(slot) = current.get_mut(idx) {
                *slot = Some(bytes);
            }
        }
        if let (Some(u), Some(v)) = (frame.plane(1), frame.plane(2)) {
            sat_hue_tags(u, v, &mut tags);
        }
        self.previous = current;
        for (key, value) in tags {
            frame.set_metadata(key, value);
        }
        frame
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(SignalStats::default())),
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
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.YMIN"), Some("16"));
        assert!(out.metadata_get("lavfi.signalstats.UMIN").is_none());
    }

    fn yuv_frame(width: u32, height: u32, luma: u8, cb: u8, cr: u8) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, width, height).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(luma);
        }
        if let Some(mut p) = f.plane_mut(1) {
            p.fill(cb);
        }
        if let Some(mut p) = f.plane_mut(2) {
            p.fill(cr);
        }
        f
    }

    /// Measured against `ffmpeg 8.1`: `color=red` under `yuv420p` is
    /// `Y=81,U=90,V=240`. `SATMAX=floor(sqrt(38^2+112^2))=118` and
    /// `HUEAVG=floor((atan2(-38,112)*180/pi+180) mod 360)=161`.
    #[test]
    fn red_matches_the_measured_saturation_and_hue() {
        let f = yuv_frame(4, 4, 81, 90, 240);
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.SATMAX"), Some("118"));
        assert_eq!(out.metadata_get("lavfi.signalstats.HUEAVG"), Some("161"));
    }

    /// Distinguishing input for `floor` vs `round` in the saturation
    /// formula: `color=green` (`U=91,V=81`) has `sqrt(37^2+47^2)=59.82`,
    /// which floors to `59` and rounds to `60` — the reference measures
    /// `59`, and red/blue's own saturations happen to floor and round
    /// identically so cannot tell the two apart on their own.
    #[test]
    fn green_saturation_floors_rather_than_rounds() {
        let f = yuv_frame(4, 4, 81, 91, 81);
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.SATMAX"), Some("59"));
    }

    /// `*DIF` is `0` before any previous frame exists.
    #[test]
    fn dif_is_zero_on_the_first_frame() {
        let f = yuv_frame(4, 4, 100, 128, 128);
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.YDIF"), Some("0"));
    }

    /// Distinguishing input: exactly half of `Y`'s samples change by `20`
    /// between frames, the other half stay put. `YDIF` (mean absolute
    /// difference over *every* sample) must be `10`, not `20` (which a
    /// "mean over only the changed samples" alternative would give).
    /// Measured against `ffmpeg 8.1`.
    #[test]
    fn dif_averages_over_every_sample_not_just_the_changed_ones() {
        let pool = FramePool::default();
        let mut first = pool.acquire_video(PixFmt::Yuv420p, 4, 4).unwrap();
        if let Some(mut p) = first.plane_mut(0) {
            p.fill(100);
        }
        if let Some(mut p) = first.plane_mut(1) {
            p.fill(128);
        }
        if let Some(mut p) = first.plane_mut(2) {
            p.fill(128);
        }
        let mut second = pool.acquire_video(PixFmt::Yuv420p, 4, 4).unwrap();
        if let Some(mut p) = second.plane_mut(0) {
            for y in 0..2 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(120);
                }
            }
            for y in 2..4 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(100);
                }
            }
        }
        if let Some(mut p) = second.plane_mut(1) {
            p.fill(128);
        }
        if let Some(mut p) = second.plane_mut(2) {
            p.fill(128);
        }
        let mut filt = SignalStats::default();
        let _ = filt.step(first);
        let out = filt.step(second);
        assert_eq!(out.metadata_get("lavfi.signalstats.YDIF"), Some("10"));
    }

    /// Distinguishing input for `BITDEPTH`: a flat plane at `100`
    /// (`0b110_0100`, three set bits) measures `3`; corrects this crate's
    /// own earlier claim (recorded in `docs/filter/vaco-filter-analysis.md`)
    /// that a constant plane always measures `1` — that was true only for
    /// the specific value `128` (`0b1000_0000`) the original fixture used.
    #[test]
    fn bitdepth_is_popcount_of_the_value_not_always_one_for_flat_planes() {
        let f = yuv_frame(4, 4, 100, 128, 128);
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.YBITDEPTH"), Some("3"));
    }

    /// Distinguishing input for `BITDEPTH`: a two-level plane holding `100`
    /// and `120` unions to `0b111_1100` (five set bits) — rules out
    /// "popcount of a single representative value" as a hypothesis, since
    /// that would give a different (and, depending on which value it
    /// picked, possibly still-plausible-looking) answer.
    #[test]
    fn bitdepth_unions_every_distinct_value_present() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, 4, 4).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            if let Some(row) = p.row_mut(0) {
                row.fill(100);
            }
            for y in 1..4 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(120);
                }
            }
        }
        if let Some(mut p) = f.plane_mut(1) {
            p.fill(128);
        }
        if let Some(mut p) = f.plane_mut(2) {
            p.fill(128);
        }
        let out = SignalStats::default().step(f);
        assert_eq!(out.metadata_get("lavfi.signalstats.YBITDEPTH"), Some("5"));
    }
}
