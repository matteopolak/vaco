//! `stabtransform` — pass 2 of a two-pass stabiliser, reading the file
//! [`crate::stabdetect`] wrote.
//!
//! See `stabdetect`'s own doc for why this is `stabtransform` and not the
//! reference's `vidstabtransform`, and for which option names are taken
//! from the reference's published documentation versus original to this
//! crate. This crate's transform file (`vaco-stab-transforms v1`) is a
//! plain-text list: a magic first line, then one `dx dy` pair per frame —
//! frame-to-frame *relative* motion, in [`crate::stabdetect`]'s own
//! measured units (luma pixels). Not `.trf`-compatible in either
//! direction.
//!
//! # Algorithm
//!
//! The whole file is read at filter creation (it is small — two `f64`s of
//! text per frame — and pass 1 has already finished writing it by the
//! time pass 2 starts, so there is no streaming concern). From the
//! per-frame relative vectors this computes the absolute camera
//! trajectory (a running sum, same shape as [`crate::deshake`]'s online
//! version) and then a **centred** moving-average smoothing of that whole
//! trajectory with window radius `smoothing` (so `smoothing=10` averages
//! 21 samples, matching the reference's own documented `value*2+1`
//! window) — the genuine two-pass advantage over `deshake`'s causal-only
//! exponential average: with the whole path known up front, the smoothed
//! path can use future frames too, not just past ones. `smoothing=0` is
//! the reference's own documented special case, "a static camera is
//! simulated": the smoothed path is held at the trajectory's own starting
//! point for every frame, which combined with `relative`'s meaning below
//! is what `tripod=1` maps onto.
//!
//! Only the reference's `optalgo=avg` (moving-average) path is
//! implemented; `optalgo=gauss` (the reference's own default) is parsed
//! but not distinguished from `avg` — a real, named scope cut, not a
//! silent substitution.
//!
//! Per frame, the correction (actual position minus smoothed position,
//! same sign convention [`crate::deshake`] already proved correct via its
//! own jitter-reduction test) is clamped to `maxshift` if set, negated
//! when `invert=1`, and applied with [`common::warp_translate`] — the
//! same translation-only warp `deshake` uses; no rotation/zoom
//! correction, the same structural gap `deshake`'s own doc names.
//! `zoom`/`optzoom`/`zoomspeed`/`interpol` are parsed for option-surface
//! completeness and do not change behaviour.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, EdgeMode};
use crate::stabdetect::FILE_MAGIC;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "stabtransform",
    description: "Video stabilization/deshaking, pass 2 of 2 (this crate's own file format, not .trf-compatible).",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "stabtransform", help = "Video stabilization/deshaking.")]
pub(crate) struct Opts {
    #[opt(name = "input", help = "set path to the file used to read the transforms", default = "transforms.trf".to_owned(), flags(video, filtering))]
    pub input: String,
    #[opt(name = "smoothing", help = "set the number of frames (value*2+1) used for lowpass filtering the camera movements", default = 10, range = 0..=1000, flags(video, filtering))]
    pub smoothing: i64,
    #[opt(name = "maxshift", help = "set maximal number of pixels to translate frames, -1 for no limit", default = -1, range = -1..=i64::MAX, flags(video, filtering))]
    pub maxshift: i64,
    #[opt(name = "invert", help = "invert transforms", default = 0, range = 0..=1, flags(video, filtering))]
    pub invert: i64,
    #[opt(name = "relative", help = "consider transforms relative to previous frame", default = 0, range = 0..=1, flags(video, filtering))]
    pub relative: i64,
    #[opt(name = "crop", help = "set border fill mode", default = "keep".to_owned(), flags(video, filtering))]
    pub crop: String,
    #[opt(name = "zoom", help = "set percentage to zoom", default = 0.0, range = -100.0..=100.0, flags(video, filtering))]
    pub zoom: f64,
    #[opt(name = "tripod", help = "enable virtual tripod mode (relative=0:smoothing=0)", default = 0, range = 0..=1, flags(video, filtering))]
    pub tripod: i64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        if o.relative != 0 {
            return Err("stabtransform: `relative` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        #[allow(
            clippy::float_cmp,
            reason = "exact comparison against this option's own literal parsed \
                      default, not a numeric-error-margin question"
        )]
        if o.zoom != 0.0 {
            return Err("stabtransform: `zoom` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

/// Reads and parses a `stabdetect`-written transform file into per-frame
/// relative `(dx, dy)` vectors.
///
/// # Errors
/// A clean message naming the path for a missing/unreadable file, a bad
/// magic line, or a line that does not parse as two floats.
pub(crate) fn read_transforms(path: &str) -> std::result::Result<Vec<(f64, f64)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("stabtransform: cannot read `{path}`: {e}"))?;
    let mut lines = text.lines();
    let magic = lines.next().unwrap_or_default();
    if magic != FILE_MAGIC {
        return Err(format!(
            "stabtransform: `{path}` is not a vaco stabiliser transform file (expected `{FILE_MAGIC}`, this crate does not read the reference's `.trf` format)"
        ));
    }
    let mut out = Vec::new();
    for (i, line) in lines.enumerate() {
        let mut parts = line.split_whitespace();
        let (Some(dx), Some(dy)) = (parts.next(), parts.next()) else {
            return Err(format!("stabtransform: `{path}` line {}: expected `dx dy`", i + 2));
        };
        let dx: f64 = dx.parse().map_err(|_| format!("stabtransform: `{path}` line {}: bad dx", i + 2))?;
        let dy: f64 = dy.parse().map_err(|_| format!("stabtransform: `{path}` line {}: bad dy", i + 2))?;
        out.push((dx, dy));
    }
    Ok(out)
}

/// Absolute camera trajectory (running sum) from per-frame relative
/// vectors.
fn trajectory_of(relative: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut pos = (0.0, 0.0);
    for &(dx, dy) in relative {
        pos.0 += dx;
        pos.1 += dy;
        out.push(pos);
    }
    out
}

/// Centred moving-average smoothing of `trajectory` with window radius
/// `radius` (so `2*radius+1` samples per point, clamped at the ends of
/// the sequence — same "use whatever exists" edge policy as the rest of
/// this crate's block search). `radius == 0` returns every point equal to
/// `trajectory[0]` — the reference's own documented "static camera"
/// special case.
fn smooth(trajectory: &[(f64, f64)], radius: usize) -> Vec<(f64, f64)> {
    if trajectory.is_empty() {
        return Vec::new();
    }
    if radius == 0 {
        let start = trajectory.first().copied().unwrap_or((0.0, 0.0));
        return trajectory.iter().map(|_| start).collect();
    }
    let n = trajectory.len();
    let mut out = Vec::new();
    for i in 0..n {
        let lo = i.saturating_sub(radius);
        let hi = (i.saturating_add(radius)).min(n.saturating_sub(1));
        #[allow(clippy::cast_precision_loss, reason = "window size is bounded by the option's own 0..=1000 range")]
        let count = (hi.saturating_sub(lo).saturating_add(1)) as f64;
        let mut sum = (0.0, 0.0);
        for point in trajectory.get(lo..=hi).unwrap_or(&[]) {
            sum.0 += point.0;
            sum.1 += point.1;
        }
        out.push((sum.0 / count, sum.1 / count));
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    corrections: Vec<(f64, f64)>,
    maxshift: Option<f64>,
    invert: bool,
    edge: EdgeMode,
    index: usize,
    checked_format: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let relative = read_transforms(&opts.input)?;
        let trajectory = trajectory_of(&relative);
        #[allow(clippy::cast_sign_loss, reason = "range is 0..=1000")]
        let radius = if opts.tripod != 0 { 0 } else { opts.smoothing as usize };
        let smoothed = smooth(&trajectory, radius);
        let corrections: Vec<(f64, f64)> = trajectory
            .iter()
            .zip(&smoothed)
            // `trajectory - smoothed`, matching `deshake`'s own sign
            // convention exactly (see this crate's `deshake::Filter`) —
            // getting this backwards makes the corrected sequence
            // *more* jittery than the raw input, not less, which is
            // exactly the bug class the shared jitter-reduction test
            // below exists to catch, and did catch here first.
            .map(|(t, s)| (t.0 - s.0, t.1 - s.1))
            .collect();
        let edge = common::EdgeMode::parse(&match opts.crop.as_str() {
            "1" | "black" => "blank".to_owned(),
            _ => "original".to_owned(),
        })?;
        Ok(Self {
            corrections,
            maxshift: (opts.maxshift >= 0).then_some(f64::from(common::to_i32(opts.maxshift))),
            invert: opts.invert != 0,
            edge,
            index: 0,
            checked_format: false,
        })
    }

    fn correction_for(&mut self) -> (f64, f64) {
        let raw = self.corrections.get(self.index).copied().unwrap_or((0.0, 0.0));
        self.index = self.index.saturating_add(1);
        let signed = if self.invert { (-raw.0, -raw.1) } else { raw };
        match self.maxshift {
            Some(m) => (signed.0.clamp(-m, m), signed.1.clamp(-m, m)),
            None => signed,
        }
    }

    /// See [`stabdetect::Filter::process`]'s doc for why this is an
    /// inherent method rather than going through [`FrameFilter`] directly
    /// in tests.
    pub(crate) fn process(&mut self, pool: &FramePool, frame: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, width, height, .. } = frame.data else {
            return Ok(FrameOut::One(frame));
        };
        if !self.checked_format {
            self.checked_format = true;
            common::ensure_8bit_addressable(format)?;
        }
        let corr = self.correction_for();
        match common::warp_translate(pool, &frame, format, width, height, corr, self.edge) {
            Some(mut warped) => {
                warped.pts = frame.pts;
                warped.time_base = frame.time_base;
                warped.duration = frame.duration;
                Ok(FrameOut::One(warped))
            }
            None => Ok(FrameOut::One(frame)),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.index = 0;
        self.checked_format = false;
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        self.process(ctx.pool(), frame)
    }

    fn flush_state(&mut self) {
        self.reset();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir().join(format!("vaco-stabtransform-test-{name}-{}", std::process::id())).to_string_lossy().into_owned()
    }

    fn write_transforms(path: &str, relative: &[(f64, f64)]) {
        use std::io::Write as _;
        let mut f = std::fs::File::create(path).unwrap();
        writeln!(f, "{FILE_MAGIC}").unwrap();
        for &(dx, dy) in relative {
            writeln!(f, "{dx} {dy}").unwrap();
        }
    }

    fn shifted_frame(w: u32, h: u32, shift: i32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    for (x, cell) in row.iter_mut().enumerate() {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "test fixture, small bounded values")]
                        let v = (((x as i32 - shift).rem_euclid(256)) as u8).wrapping_add((y * 7) as u8);
                        *cell = v;
                    }
                }
            }
        }
        f
    }

    #[test]
    fn smoothing_zero_locks_every_frame_to_the_start() {
        let trajectory = vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (3.0, 0.0)];
        let smoothed = smooth(&trajectory, 0);
        for s in smoothed {
            assert_eq!(s, (0.0, 0.0));
        }
    }

    #[test]
    fn centred_smoothing_uses_future_samples_a_causal_average_cannot() {
        // A single spike surrounded by zeros: a *causal* average (like
        // deshake's own EMA) only starts reacting once the spike has
        // already happened. A centred average sees it coming and going,
        // so the smoothed value at the spike itself is pulled down by
        // its still-zero neighbours on both sides -- the genuine
        // two-pass advantage this filter exists to provide.
        let trajectory = vec![(0.0, 0.0), (0.0, 0.0), (100.0, 0.0), (0.0, 0.0), (0.0, 0.0)];
        let smoothed = smooth(&trajectory, 2);
        let spike_smoothed = smoothed[2].0;
        assert!(spike_smoothed > 0.0 && spike_smoothed < 100.0, "expected a damped, nonzero value, got {spike_smoothed}");
    }

    #[test]
    fn jittery_sequence_is_smoothed_more_than_the_raw_input() {
        let (w, h) = (64u32, 64u32);
        let jitters = [0i32, 6, -6, 6, -6, 6, -6];
        let raw: Vec<Frame> = jitters.iter().map(|&s| shifted_frame(w, h, s)).collect();

        // Relative motion between consecutive frames matches `jitters`'
        // own differences, exactly what stabdetect would have measured
        // from this same sequence; the first frame has no predecessor, so
        // its own relative motion is `0`.
        let mut relative = vec![(0.0, 0.0)];
        relative.extend(jitters.windows(2).map(|w2| (f64::from(w2[1] - w2[0]), 0.0)));
        let path = tmp_path("jitter");
        write_transforms(&path, &relative);

        let opts = Opts { input: path.clone(), smoothing: 3, ..Opts::default() };
        let mut filt = Filter::new(&opts).unwrap();
        let pool = FramePool::default();
        let corrected: Vec<Frame> = raw
            .iter()
            .map(|f| match filt.process(&pool, f.clone()).unwrap() {
                FrameOut::One(out) => out,
                _ => panic!("expected exactly one output frame"),
            })
            .collect();

        let raw_diff: u64 = raw.windows(2).map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap())).sum();
        let corrected_diff: u64 = corrected.windows(2).map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap())).sum();

        assert!(
            corrected_diff < raw_diff,
            "expected stabilisation to reduce total frame-to-frame difference: raw={raw_diff} corrected={corrected_diff}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_magic_is_a_clean_error() {
        let path = tmp_path("badmagic");
        std::fs::write(&path, "not-a-vaco-file\n1.0 2.0\n").unwrap();
        assert!(read_transforms(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_a_clean_error() {
        let req = Instantiate {
            name: "stabtransform",
            instance: "stabtransform",
            args: Some("input=/nonexistent-vaco-test-transforms.trf"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
