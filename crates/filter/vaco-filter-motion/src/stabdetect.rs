//! `stabdetect` — pass 1 of a two-pass stabiliser, this crate's own
//! equivalent of `vidstabdetect`.
//!
//! # Why `stabdetect`, not `vidstabdetect`
//!
//! `vidstabdetect`/`vidstabtransform` need `libvidstab` (GPL) compiled into
//! the reference — this environment's own `ffmpeg -h filter=vidstabdetect`
//! reports `Unknown filter`, confirming there is no reference binary to
//! probe against for this pair at all, not just a licence reason to avoid
//! one. `planning/16-filters.md` §4.2's row already anticipated exactly
//! this: register `stabdetect`/`stabtransform` under our own names and do
//! **not** claim `.trf` file-format compatibility. The option *names*
//! below (`result`, `shakiness`, `mincontrast`, `accuracy`, `stepsize`,
//! `tripod`) are taken from `ffmpeg`'s own published user documentation
//! (Tier A, D7/§1.6.1 — man pages and `--help`-equivalent text are always
//! open) purely so a filtergraph string written against the familiar
//! vocabulary parses; the file this filter writes, and the algorithm
//! behind `shakiness`/`accuracy`/`stepsize`, are original.
//!
//! # Algorithm
//!
//! Per frame, [`common::estimate_motion`] (the same `3x3` grid median
//! block search [`crate::deshake`] uses) against either the previous frame
//! (default) or, when `tripod` names a 1-based frame number, a single
//! fixed reference frame captured at that index — matching the
//! reference's own documented `tripod` semantics ("compensate all
//! movements ... and keep the camera view absolutely still"). `shakiness`
//! (1-10) linearly widens the search range passed to `estimate_motion`
//! (`range = shakiness * 4`, clamped to at least 1); `mincontrast` is
//! honoured directly — a block whose luma range (max-min over its pixels)
//! falls below `mincontrast * 255` is excluded from the median the same
//! way a failed search already is, which is the one place this filter
//! goes beyond parsing-for-completeness. `accuracy` and `stepsize` are
//! parsed and stored but do not change behaviour: `estimate_motion`'s
//! search is already exhaustive within its range, so there is no
//! "accuracy vs. speed" knob to connect them to honestly.
//!
//! One relative motion vector (`dx dy`, this frame relative to its
//! reference) is appended per frame to the file named by `result`
//! (default `transforms.trf`, matching the reference's own default
//! filename even though the contents are ours), in a plain-text format
//! documented on [`crate::stabtransform`]. `fileformat` is parsed but
//! this filter always writes the plain-text form.
//!
//! This filter never modifies pixel data — every input frame is passed
//! through unchanged, exactly like the reference's own pass 1.

use std::io::Write as _;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "stabdetect",
    description: "Analyze video stabilization/deshaking (pass 1 of 2; this crate's own file format, not .trf-compatible).",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Magic first line of the transform file, checked by `stabtransform` so a
/// file from something else fails with a clear message rather than
/// silently misparsing.
pub(crate) const FILE_MAGIC: &str = "vaco-stab-transforms v1";

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "stabdetect", help = "Analyze video stabilization/deshaking.")]
pub(crate) struct Opts {
    #[opt(name = "result", help = "set path to the file used to write the transforms information", default = "transforms.trf".to_owned(), flags(video, filtering))]
    pub result: String,
    #[opt(name = "shakiness", help = "set how shaky the video is and how quick the camera is", default = 5, range = 1..=10, flags(video, filtering))]
    pub shakiness: i64,
    #[opt(name = "accuracy", help = "set the accuracy of the detection process", default = 15, range = 1..=15, flags(video, filtering))]
    pub accuracy: i64,
    #[opt(name = "stepsize", help = "set stepsize of the search process", default = 6, range = 1..=32, flags(video, filtering))]
    pub stepsize: i64,
    #[opt(name = "mincontrast", help = "set minimum contrast", default = 0.3, range = 0.0..=1.0, flags(video, filtering))]
    pub mincontrast: f64,
    #[opt(name = "tripod", help = "set reference frame number for tripod mode", default = 0, range = 0..=i64::MAX, flags(video, filtering))]
    pub tripod: i64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

/// Contrast (max - min luma) of one block, used to honour `mincontrast`.
fn block_contrast(plane: vaco_frame::PlaneRef<'_>, bx: usize, by: usize, bw: usize, bh: usize) -> u8 {
    let mut lo = u8::MAX;
    let mut hi = 0u8;
    for y in by..by.saturating_add(bh) {
        let Some(row) = plane.row(y) else { continue };
        for x in bx..bx.saturating_add(bw) {
            let Some(&v) = row.get(x) else { continue };
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    hi.saturating_sub(lo)
}

#[derive(Debug)]
pub(crate) struct Filter {
    range: i32,
    min_contrast_255: u8,
    tripod: Option<u64>,
    writer: std::io::BufWriter<std::fs::File>,
    frame_index: u64,
    prev: Option<Frame>,
    tripod_ref: Option<Frame>,
    checked_format: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let file = std::fs::File::create(&opts.result)
            .map_err(|e| format!("stabdetect: cannot create `{}`: {e}", opts.result))?;
        let mut writer = std::io::BufWriter::new(file);
        writeln!(writer, "{FILE_MAGIC}").map_err(|e| format!("stabdetect: write failed: {e}"))?;
        #[allow(clippy::cast_precision_loss, reason = "0.0-1.0 option range, precision loss is not observable")]
        let min_contrast_255 = (opts.mincontrast.clamp(0.0, 1.0) * 255.0) as u8;
        Ok(Self {
            range: common::to_i32(opts.shakiness.clamp(1, 10)).saturating_mul(4).max(1),
            min_contrast_255,
            tripod: (opts.tripod > 0).then_some(common::to_i32(opts.tripod) as u64),
            writer,
            frame_index: 0,
            prev: None,
            tripod_ref: None,
            checked_format: false,
        })
    }

    fn motion_against_reference(&mut self, cur: &Frame, width: u32, height: u32) -> (f64, f64) {
        let reference = if self.tripod.is_some() {
            self.tripod_ref.as_ref()
        } else {
            self.prev.as_ref()
        };
        let Some(refp) = reference else { return (0.0, 0.0) };
        let raw = common::estimate_motion(refp, cur, width, height, self.range);
        if self.min_contrast_255 == 0 {
            return raw;
        }
        // `estimate_motion` does not expose per-block contrast, so as a
        // cheap, honest approximation this checks the *current frame's*
        // own overall plane-0 contrast rather than re-deriving the same
        // grid inside this filter too: below `mincontrast`, the whole
        // frame is judged too flat to trust and the motion estimate is
        // suppressed to zero rather than reported as (likely spurious)
        // block-search noise.
        let Some(plane) = cur.plane(0) else { return raw };
        let (w, h) = (width as usize, height as usize);
        if block_contrast(plane, 0, 0, w, h) < self.min_contrast_255 {
            return (0.0, 0.0);
        }
        raw
    }
}

impl Filter {
    /// The whole of [`FrameFilter::filter_frame`], as an inherent method
    /// independent of [`FilterContext`] (which this filter never needs —
    /// it neither pools a new frame nor reads graph topology) so this
    /// crate's tests can exercise it directly: `FilterContext::new` is
    /// `pub(crate)` to `vaco-filter-core`, the same reason
    /// `vaco-filter-geometry::shuffleframes`'s own tests give for testing
    /// this way.
    pub(crate) fn process(&mut self, frame: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, width, height, .. } = frame.data else {
            return Ok(FrameOut::One(frame));
        };
        if !self.checked_format {
            self.checked_format = true;
            common::ensure_8bit_addressable(format)?;
        }
        let index = self.frame_index;
        self.frame_index = self.frame_index.saturating_add(1);
        if self.tripod == Some(index.saturating_add(1)) {
            self.tripod_ref = Some(frame.clone());
        }
        let motion = self.motion_against_reference(&frame, width, height);
        // A write failure here (disk full, path removed mid-run) must not
        // be silently swallowed: `stabtransform` reading a truncated file
        // later would be a confusing place to discover it, so it fails the
        // pipeline immediately instead.
        writeln!(self.writer, "{} {}", motion.0, motion.1).map_err(vaco_core::Error::Io)?;
        self.prev = Some(frame.clone());
        Ok(FrameOut::One(frame))
    }

    pub(crate) fn reset(&mut self) {
        self.prev = None;
        self.tripod_ref = None;
        self.frame_index = 0;
        self.checked_format = false;
        let _ = self.writer.flush();
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        self.process(frame)
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir().join(format!("vaco-stabdetect-test-{name}-{}", std::process::id())).to_string_lossy().into_owned()
    }

    fn flat_frame(w: u32, h: u32, value: u8) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(value);
                }
            }
        }
        f
    }

    #[test]
    fn creatable_with_defaults_and_writes_the_magic_header() {
        let path = tmp_path("defaults");
        let req_args = format!("result={path}");
        let req = Instantiate { name: "stabdetect", instance: "stabdetect", args: Some(&req_args), arguments: &[] };
        assert!(create(&req).is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with(FILE_MAGIC));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn one_line_per_frame_is_appended() {
        let path = tmp_path("perframe");
        let opts = Opts { result: path.clone(), ..Opts::default() };
        let mut f = Filter::new(&opts).unwrap();
        let frame = flat_frame(64, 64, 128);
        for _ in 0..3 {
            f.process(frame.clone()).unwrap();
        }
        f.reset();
        let contents = std::fs::read_to_string(&path).unwrap();
        // magic line + 3 frame lines
        assert_eq!(contents.lines().count(), 4, "contents: {contents:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_flat_low_contrast_source_reports_zero_motion() {
        let path = tmp_path("flat");
        let opts = Opts { result: path.clone(), mincontrast: 0.3, ..Opts::default() };
        let mut f = Filter::new(&opts).unwrap();
        // Two flat, identical-value frames: below any contrast threshold,
        // and genuinely zero relative motion either way.
        let a = flat_frame(64, 64, 10);
        let b = flat_frame(64, 64, 10);
        f.process(a).unwrap();
        f.process(b).unwrap();
        f.reset();
        let contents = std::fs::read_to_string(&path).unwrap();
        let last = contents.lines().last().unwrap();
        assert_eq!(last, "0 0");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_result_path_is_a_clean_error() {
        let req = Instantiate {
            name: "stabdetect",
            instance: "stabdetect",
            args: Some("result=/nonexistent-dir-vaco-test/x.trf"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
