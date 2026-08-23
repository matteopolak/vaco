//! `idet` — a passthrough analysis filter that classifies each frame as
//! progressive or interlaced.
//!
//! `ffmpeg -h filter=idet`: `intl_thres`/`prog_thres`/`rep_thres` (float
//! thresholds), `half_life`, `analyze_interlaced_flag`. This crate parses
//! all five for option-table completeness; only a normalised-threshold
//! classification is implemented (below).
//!
//! # Metadata export: interface gap 11, closed while this crate was in flight
//!
//! The reference reports its per-frame classification and cumulative
//! counts as frame-attached dictionary entries — measured directly
//! (`ffprobe -show_frames -f lavfi -i testsrc2,idet`, ffmpeg 8.1,
//! 2026-08-23): `lavfi.idet.single.current_frame` (`tff`/`bff`/
//! `progressive`/`undetermined`), `.multiple.current_frame`,
//! `.repeated.current_frame` (`neither`/`top`/`bottom`), plus per-category
//! running fractions (`.single.tff`, `.repeated.neither`, etc.).
//! `vaco_frame::Frame` had no open-ended per-frame metadata dictionary when
//! this crate started (`INTERFACE-GAPS.md` gap 11, reported by the
//! `vaco-filter-color`/`-key`/`-lut` agent), but it closed additively
//! (`Frame::set_metadata`/`metadata_get`, a new `FrameSideData::Metadata`
//! variant) before this crate finished — checked again immediately before
//! this module was written, per that gap's own note that `idet` was
//! expected to need it. So this filter **does** write real
//! `lavfi.idet.*` keys, under the real names, onto every output frame —
//! [`Filter::classify`] calls `Frame::set_metadata` directly. What it does
//! not reproduce is the reference's *vocabulary*: this classifier only
//! distinguishes progressive from interlaced (no `tff`/`bff` parity split,
//! no `undetermined`, no cumulative `.multiple.*`/per-category fractions),
//! so `lavfi.idet.single.current_frame` here reads `progressive` or
//! `interlaced` rather than one of the reference's four values — a
//! narrower vocabulary under the correct key, not a missing channel.
//! [`Filter::classification`] additionally exposes the same running tallies
//! as a plain accessor, for this module's own tests.
//!
//! # Algorithm
//!
//! [`vaco_filter_vdsp::comb_score`] on the luma plane, normalised by pixel
//! count, is this filter's per-frame combing metric — an **original**
//! metric (see that crate's doc), not the reference's own detector, which
//! this project cannot read (D7). A frame scores `Interlaced` when the
//! normalised score exceeds a fixed threshold, `Progressive` otherwise. A
//! frame whose luma is byte-identical to the immediately preceding one
//! (`vaco_filter_vdsp::plane_sad` is exactly `0`) is additionally counted
//! `Repeated`, independent of its progressive/interlaced classification.
//!
//! # Independent oracle
//!
//! A synthetic stream built by feeding the *same* progressive frame
//! repeatedly (zero vertical second difference, by the algebraic identity
//! [`vaco_filter_vdsp::comb_score`]'s own doc names) must classify every
//! frame `Progressive`; a synthetic stream of frames whose rows strictly
//! alternate between two fixed values (the textbook combing pattern) must
//! classify every frame `Interlaced` — both checked directly against
//! [`Filter::classification`]'s tallies, not against this filter's own
//! per-frame decision re-examined a second way.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "idet",
    description: "Interlace detect Filter.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "idet", help = "Interlace detect Filter")]
pub(crate) struct Opts {
    #[opt(name = "intl_thres", help = "set interlacing threshold", default = 1.04, flags(video, filtering))]
    pub intl_thres: f64,
    #[opt(name = "prog_thres", help = "set progressive threshold", default = 1.5, flags(video, filtering))]
    pub prog_thres: f64,
    #[opt(name = "rep_thres", help = "set repeat threshold", default = 3.0, flags(video, filtering))]
    pub rep_thres: f64,
    #[opt(name = "half_life", help = "half life of cumulative statistics", default = 0.0, flags(video, filtering))]
    pub half_life: f64,
    #[opt(
        name = "analyze_interlaced_flag",
        help = "frames to use to determine if the interlace flag is accurate",
        default = 0,
        flags(video, filtering)
    )]
    pub analyze_interlaced_flag: i32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Progressive,
    Interlaced,
}

/// Running tallies. `pub(crate)`: this concrete filter type is reached only
/// through the boxed `dyn Filter` the registry hands back, so a wider
/// visibility would be unreachable dead API — the same reasoning
/// `freezedetect::Filter::events` documents.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Tally {
    pub(crate) progressive: u64,
    pub(crate) interlaced: u64,
    pub(crate) repeated: u64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    threshold: f64,
    prev_luma: Option<Frame>,
    tally: Tally,
}

impl Filter {
    pub(crate) fn new(threshold: f64) -> Self {
        Self {
            threshold,
            prev_luma: None,
            tally: Tally::default(),
        }
    }

    #[allow(dead_code, reason = "exercised by this module's tests; see the module doc")]
    pub(crate) const fn classification(&self) -> Tally {
        self.tally
    }

    fn classify(&mut self, frame: &mut Frame) -> Class {
        let Some(plane) = frame.plane(0) else {
            return Class::Progressive;
        };
        let rows = plane.rows();
        let cols = plane.row(0).map_or(0, <[u8]>::len);
        let samples = rows.saturating_sub(2).saturating_mul(cols).max(1);
        #[allow(clippy::cast_precision_loss, reason = "display-scale normalisation")]
        let normalised = vaco_filter_vdsp::comb_score(plane) as f64 / samples as f64;
        let repeated = self.prev_luma.as_ref().is_some_and(|prev| {
            prev.plane(0)
                .is_some_and(|prev_plane| vaco_filter_vdsp::plane_sad(plane, prev_plane) == 0)
        });
        if repeated {
            self.tally.repeated = self.tally.repeated.saturating_add(1);
        }
        self.prev_luma = Some(frame.clone());
        let class = if normalised > self.threshold {
            self.tally.interlaced = self.tally.interlaced.saturating_add(1);
            Class::Interlaced
        } else {
            self.tally.progressive = self.tally.progressive.saturating_add(1);
            Class::Progressive
        };
        // Interface gap 11 (`vaco_frame::Frame` metadata dictionary) closed
        // 2026-08-23, before this crate finished — see the module doc.
        // Real keys measured via `ffprobe -show_frames -f lavfi
        // -i testsrc2,idet`: `lavfi.idet.single.current_frame` and
        // `lavfi.idet.repeated.current_frame` are the two this filter can
        // answer. The reference's vocabulary for the former is
        // `tff`/`bff`/`progressive`/`undetermined` (four-way, from a
        // spatio-temporal parity analysis this crate does not implement);
        // this classifier only distinguishes two of those, so it publishes
        // `progressive`/`interlaced` under the same key rather than
        // fabricating a `tff`/`bff` split it cannot support — a documented,
        // narrower vocabulary under the real key name.
        let current = match class {
            Class::Progressive => "progressive",
            Class::Interlaced => "interlaced",
        };
        frame.set_metadata("lavfi.idet.single.current_frame", current);
        frame.set_metadata(
            "lavfi.idet.repeated.current_frame",
            if repeated { "repeated" } else { "neither" },
        );
        class
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, mut frame: Frame) -> Result<FrameOut> {
        let _ = ctx;
        let _ = self.classify(&mut frame);
        Ok(FrameOut::One(frame))
    }

    fn flush_state(&mut self) {
        self.prev_luma = None;
        self.tally = Tally::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts.intl_thres))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::ramp_frame;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn combed_frame() -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 8, 16).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..16usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(if y % 2 == 0 { 0 } else { 255 });
                }
            }
        }
        f
    }

    #[test]
    fn a_static_progressive_stream_classifies_progressive() {
        let mut filt = Filter::new(1.04);
        for _ in 0..5 {
            let _ = filt.classify(&mut ramp_frame(8, 16));
        }
        let t = filt.classification();
        assert_eq!(t.interlaced, 0, "a smooth ramp must never classify interlaced");
        assert_eq!(t.progressive, 5);
    }

    #[test]
    fn a_strictly_alternating_stream_classifies_interlaced() {
        let mut filt = Filter::new(1.04);
        for _ in 0..5 {
            let _ = filt.classify(&mut combed_frame());
        }
        let t = filt.classification();
        assert_eq!(t.progressive, 0, "strict row alternation must never classify progressive");
        assert_eq!(t.interlaced, 5);
    }

    #[test]
    fn identical_frames_count_as_repeated() {
        let mut filt = Filter::new(1.04);
        let mut f = ramp_frame(8, 16);
        let _ = filt.classify(&mut f.clone());
        let _ = filt.classify(&mut f);
        assert_eq!(filt.classification().repeated, 1);
    }

    #[test]
    fn real_lavfi_idet_keys_are_published_on_the_output_frame() {
        // Interface gap 11 closed while this crate was in flight: real
        // `lavfi.idet.*` keys, under the reference's own names, now land on
        // the output frame via `Frame::set_metadata`.
        let mut filt = Filter::new(1.04);
        let mut f = ramp_frame(8, 16);
        let class = filt.classify(&mut f);
        assert_eq!(class, Class::Progressive);
        assert_eq!(f.metadata_get("lavfi.idet.single.current_frame"), Some("progressive"));
        assert_eq!(f.metadata_get("lavfi.idet.repeated.current_frame"), Some("neither"));
    }
}
