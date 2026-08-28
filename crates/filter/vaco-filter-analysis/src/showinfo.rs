//! `showinfo` — print per-frame diagnostics at info log level.
//!
//! Reported (interface gap 13, `planning/INTERFACE-GAPS.md`) as needing a
//! console-log-only side channel: `showinfo` writes **no** frame metadata,
//! measured directly against `ffmpeg 8.1` (`ffprobe -show_frames` through
//! it emits no `"tags"` block at all) — its entire output is two log lines
//! per frame. [`vaco_frame::FrameSideData::Log`] is that channel, closed
//! alongside this filter as its first real consumer.
//!
//! # Measured line format
//!
//! Against a `4x2` `yuv420p` frame from `ffmpeg 8.1`'s own `color`/`testsrc`
//! lavfi sources (`-vf showinfo`, `-loglevel info`), byte for byte:
//!
//! ```text
//! n:   0 pts:      0 pts_time:0       duration:      1 duration_time:1
//!   fmt:yuv420p cl:unspecified sar:1/1 s:4x2 i:P iskey:1 type:I
//!   checksum:1ACA051C plane_checksum:[0B640288 010E00B4 02D001E0]
//!   mean:[81 90 240] stdev:[0.0 0.0 0.0]
//! color_range:unknown color_space:unknown color_primaries:unknown color_trc:unknown
//! ```
//!
//! (wrapped here for width; the reference emits `n:` through `stdev:` as one
//! line and the four `color_*` fields as a second).
//!
//! Field by field, each independently measured:
//!
//! * `n` — `%4d`, this filter's own per-instance frame counter starting at 0.
//! * `pts`/`duration` — `%7d`, right-aligned, the raw tick count.
//! * `pts_time`/`duration_time` — `%-7s`, left-aligned, [`crate::fmt::trimmed_time`]
//!   (`freezedetect`'s six-decimals-trailing-zeros-trimmed rule) applied to
//!   `ticks * time_base`.
//! * `fmt` — [`vaco_pixfmt::PixFmt::name`].
//! * `cl` — chroma location (`ffmpeg -vf setparams=chroma_location=left`
//!   confirms the field, since `-vf setrange=` does *not* move it — `cl` is
//!   chroma siting, not colour range, despite the abbreviation).
//! * `sar` — `Rational`'s own `num/den` `Display`.
//! * `s` — `{width}x{height}`.
//! * `i` — `P` progressive, `T`/`B` top/bottom-field-first interlaced
//!   (`T` confirmed via `tinterlace`; `B` inferred by symmetry, not
//!   independently probed).
//! * `iskey` — `0`/`1` from [`vaco_frame::FrameFlags::KEY`].
//! * `type` — always `I` here: this workspace has no decoder that attaches
//!   a picture type to a `Frame` (D5), and every source/filter-generated
//!   frame this workspace can produce is exactly what the reference itself
//!   reports as `I` for the same reason (measured on `color`, `testsrc` and
//!   `nullsrc`, all of which are `type:I` regardless of content or
//!   `iskey`). Not reachable for a real P/B decode until a decoder exists.
//! * `checksum`/`plane_checksum` — Adler-32, **`(a=0, b=0)` seeded**, not
//!   the RFC 1950 default — the same seed `planning/AGENT-CONSTRAINTS.md`
//!   already recorded for `framecrc`/`framehash`, confirmed independently
//!   here by reproducing `1ACA051C`/`[0B640288 010E00B4 02D001E0]` from the
//!   raw plane bytes. Computed over each plane's *logical* bytes
//!   ([`vaco_frame::PlaneRef::row_bytes`]-trimmed rows concatenated), never
//!   stride padding.
//! * `mean`/`stdev` — per-plane sample mean (rounded to the nearest
//!   integer) and **population** standard deviation (divide by `n`, not
//!   `n-1` — confirmed against a non-uniform fixture where the two formulas
//!   disagree and only the population one matched), one decimal place.
//! * `color_range`/`color_space`/`color_primaries`/`color_trc` — the second
//!   line, straight from [`vaco_color::ColorRange::name`],
//!   [`vaco_color::MatrixCoefficients::name`],
//!   [`vaco_color::ColorPrimaries::name`], [`vaco_color::TransferCharacteristic::name`]
//!   (all four already implement exactly this naming for `ffprobe`'s own
//!   `-show_streams` fields).
//!
//! # Scope
//!
//! `mean`/`stdev`/`checksum` are measured and implemented for 8-bit-per-
//! component formats only (every fixture probed was `yuv420p`). A 16-bit
//! format would need the reference's own sample-vs-byte convention checked
//! before extending this — not assumed here.
//!
//! `config in`/`config out time_base`/`frame_rate` — the two lines the
//! reference prints once at graph configuration, not per frame — are not
//! reproduced: they describe the *link*, not the frame, and this
//! workspace's filter model has no per-frame hook that runs before the
//! first frame arrives with that information available in the same place a
//! per-`Frame` side-data write would fit.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FrameFlags};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::trimmed_time;
use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "showinfo",
    description: "Show textual information for each video frame.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct ShowInfo {
    n: u64,
}

impl ShowInfo {
    fn step(&mut self, mut frame: Frame) -> Frame {
        let FrameData::Video { format, width, height, .. } = frame.data else {
            return frame;
        };
        let plane_count = frame.plane_count();

        let pts_ticks = frame.pts.ticks();
        let tb = frame.time_base;
        #[allow(clippy::cast_precision_loss, reason = "tick counts here are frame-scale, far below 2^53")]
        let seconds = |ticks: i64| -> String {
            if tb.den == 0 {
                return "N/A".to_owned();
            }
            trimmed_time(ticks as f64 * f64::from(tb.num) / f64::from(tb.den))
        };
        let pts_str = pts_ticks.map_or_else(|| "-9223372036854775808".to_owned(), |t| t.to_string());
        let pts_time = pts_ticks.map_or_else(|| "N/A".to_owned(), seconds);
        let duration_str = frame.duration.0.to_string();
        let duration_time = seconds(frame.duration.0);

        let interlaced = frame.flags.contains(FrameFlags::INTERLACED);
        let i_field = if !interlaced {
            'P'
        } else if frame.flags.contains(FrameFlags::TOP_FIELD_FIRST) {
            'T'
        } else {
            'B'
        };
        let iskey = u8::from(frame.flags.contains(FrameFlags::KEY));

        let mut checksum_bytes = Vec::new();
        let mut plane_checksums = Vec::new();
        let mut means = Vec::new();
        let mut stdevs = Vec::new();
        for idx in 0..plane_count {
            let Some(plane) = frame.plane(idx) else { continue };
            let mut bytes = Vec::new();
            for row in plane.rows_iter() {
                bytes.extend_from_slice(row);
            }
            plane_checksums.push(format!("{:08X}", vaco_hash::adler32_seeded(&bytes, 0, 0)));
            let (mean, stdev) = mean_stdev(&bytes);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "mean of u8 samples fits comfortably in i64")]
            means.push(mean.round() as i64);
            stdevs.push(format!("{stdev:.1}"));
            checksum_bytes.extend_from_slice(&bytes);
        }
        let checksum = format!("{:08X}", vaco_hash::adler32_seeded(&checksum_bytes, 0, 0));

        let line = format!(
            "n:{n:>4} pts:{pts:>7} pts_time:{pts_time:<7} duration:{duration:>7} duration_time:{duration_time:<7} fmt:{fmt} cl:{cl} sar:{sar} s:{w}x{h} i:{i} iskey:{iskey} type:I checksum:{checksum} plane_checksum:[{plane_checksums}] mean:[{means}] stdev:[{stdevs}]",
            n = self.n,
            pts = pts_str,
            duration = duration_str,
            fmt = format.name(),
            cl = frame.color.chroma_location.name(),
            sar = frame.sample_aspect_ratio,
            w = width,
            h = height,
            i = i_field,
            plane_checksums = plane_checksums.join(" "),
            means = means.iter().map(i64::to_string).collect::<Vec<_>>().join(" "),
            stdevs = stdevs.join(" "),
        );
        let color_line = format!(
            "color_range:{} color_space:{} color_primaries:{} color_trc:{}",
            frame.color.range.name(),
            frame.color.matrix.name(),
            frame.color.primaries.name(),
            frame.color.transfer.name(),
        );

        frame.push_log_line(line);
        frame.push_log_line(color_line);
        self.n += 1;
        frame
    }
}

/// `(mean, population standard deviation)` over one plane's raw bytes.
///
/// Measured, not the textbook sample (`n-1`) formula — see this module's
/// doc for the fixture that distinguishes them.
fn mean_stdev(bytes: &[u8]) -> (f64, f64) {
    if bytes.is_empty() {
        return (0.0, 0.0);
    }
    #[allow(clippy::cast_precision_loss, reason = "plane byte counts are far below 2^53")]
    let n = bytes.len() as f64;
    #[allow(clippy::cast_precision_loss, reason = "individual samples are 0..=255")]
    let sum: f64 = bytes.iter().map(|&b| f64::from(b)).sum();
    let mean = sum / n;
    #[allow(clippy::cast_precision_loss, reason = "individual samples are 0..=255")]
    let variance: f64 = bytes.iter().map(|&b| (f64::from(b) - mean).powi(2)).sum::<f64>() / n;
    (mean, variance.sqrt())
}

impl FrameFilter for ShowInfo {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(ShowInfo::default())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Rational;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn video_frame(w: u32, h: u32) -> Frame {
        FramePool::default().acquire_video(PixFmt::Yuv420p, w, h).unwrap()
    }

    /// Measured: `ffmpeg 8.1`, `-f lavfi -i "color=c=red:s=4x2:d=1:r=1,
    /// format=yuv420p" -vf showinfo`, first (only) frame.
    #[test]
    fn matches_the_reference_line_for_a_uniform_frame() {
        let mut frame = video_frame(4, 2);
        {
            let mut p = frame.plane_mut(0).unwrap();
            for y in 0..p.rows() {
                p.row_mut(y).unwrap().fill(0x51);
            }
        }
        {
            let mut p = frame.plane_mut(1).unwrap();
            p.row_mut(0).unwrap().fill(0x5a);
        }
        {
            let mut p = frame.plane_mut(2).unwrap();
            p.row_mut(0).unwrap().fill(0xf0);
        }
        frame.pts = vaco_core::Timestamp::new(0);
        frame.time_base = Rational::new(1, 1);
        frame.duration = vaco_core::Duration(1);

        let mut f = ShowInfo::default();
        let out = f.step(frame);
        assert_eq!(out.log_lines().len(), 2);
        assert_eq!(
            out.log_lines()[0],
            "n:   0 pts:      0 pts_time:0       duration:      1 duration_time:1       \
             fmt:yuv420p cl:unspecified sar:1/1 s:4x2 i:P iskey:0 type:I \
             checksum:1ACA051C plane_checksum:[0B640288 010E00B4 02D001E0] \
             mean:[81 90 240] stdev:[0.0 0.0 0.0]"
        );
        assert_eq!(
            out.log_lines()[1],
            "color_range:unknown color_space:unknown color_primaries:unknown color_trc:unknown"
        );
    }

    #[test]
    fn n_increments_per_frame() {
        let mut f = ShowInfo::default();
        let f1 = video_frame(2, 2);
        let f2 = video_frame(2, 2);
        let out1 = f.step(f1);
        let out2 = f.step(f2);
        assert!(out1.log_lines()[0].starts_with("n:   0"));
        assert!(out2.log_lines()[0].starts_with("n:   1"));
    }

    #[test]
    fn interlaced_top_field_first_reports_t() {
        let mut frame = video_frame(2, 2);
        frame.flags |= FrameFlags::INTERLACED | FrameFlags::TOP_FIELD_FIRST;
        let mut f = ShowInfo::default();
        let out = f.step(frame);
        assert!(out.log_lines()[0].contains(" i:T "));
    }

    #[test]
    fn a_key_frame_reports_iskey_one() {
        let mut frame = video_frame(2, 2);
        frame.flags |= FrameFlags::KEY;
        let mut f = ShowInfo::default();
        let out = f.step(frame);
        assert!(out.log_lines()[0].contains(" iskey:1 "));
    }

    /// Population stdev, not sample: a fixture where the two formulas give
    /// visibly different answers, checked against the closed-form value.
    #[test]
    fn stdev_is_population_not_sample() {
        let bytes = [0u8, 0, 0, 100];
        let (mean, stdev) = mean_stdev(&bytes);
        assert!((mean - 25.0).abs() < 1e-9);
        // population variance = mean((x-mean)^2) = (3*625 + 5625)/4 = 1875
        assert!((stdev - 1875.0_f64.sqrt()).abs() < 1e-9);
    }
}
