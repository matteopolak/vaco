//! `-fps_mode` (CL-21, #222): `passthrough`/`cfr`/`vfr`/`auto`.
//!
//! # The value set is four, not five
//!
//! Measured directly against the reference (D6): `ffmpeg 9.0.1 -fps_mode
//! drop …` is refused outright — `Invalid value drop specified for fps_mode
//! of #0:0.`, exit before opening any output — while `passthrough`, `cfr`,
//! `vfr` and `auto` are all accepted. `drop` is not a real `-fps_mode` value
//! on this reference build; [`FpsMode::parse`] only accepts the four that
//! measured true.
//!
//! # What this implements, and how
//!
//! `planning/14-cli.md` §6.4's Stage IV table is the design basis:
//!
//! | mode | behaviour |
//! |---|---|
//! | `passthrough` | timestamps forwarded unchanged; no dup, no drop |
//! | `cfr` | duplicate/drop frames to land exactly on a `1/r` grid |
//! | `vfr` | forward timestamps; drop a frame whose timestamp equals the previous one |
//! | `auto` (default) | `cfr` if the muxer wants constant rate, else `vfr` |
//!
//! `passthrough` needs no pipeline change at all: nothing in this build's
//! decode→encode leg duplicates or drops a frame today, so `passthrough` is
//! simply what already happens — [`insert`] returns the tap unchanged.
//!
//! `cfr` and `vfr` are real, working filter nodes, inserted with
//! [`vaco_sched::spec::PipelineSpec::add_filter`] between the decode leg and
//! the encoder, built directly against [`vaco_filter_core::sched::Graph`]'s
//! own `add`/`add_source`/`add_sink`/`connect` rather than through `-vf`'s
//! text grammar: neither is a real `ffmpeg` `-vf` filter (the reference's own
//! `-fps_mode` is CLI-level logic in `ffmpeg.c`, not a `libavfilter` graph
//! node), so registering either under a name in a filter crate that otherwise
//! mirrors real `libavfilter` filters one to one would be a false claim about
//! what that crate is.
//!
//! * `cfr` reuses [`vaco_filter_video_format::fps::Filter`] — the already
//!   real, tested zero-order-hold state machine behind `-vf fps=<rate>` —
//!   via [`vaco_filter_video_format::fps::Filter::from_rate`], targeting the
//!   stream's own declared frame rate (`-r` is not implemented in this build,
//!   so there is no other target to give it; the reference's own `cfr`
//!   without `-r` uses the same declared rate). **This is a defensible
//!   approximation, not a byte-identical reproduction**: the reference's own
//!   `-fps_mode cfr` duplicate/drop decision is separate code
//!   (`ffmpeg.c`'s `do_video_out`) from the `fps` filter's slot-rounding
//!   algorithm, and the two are not guaranteed to land on the same frame at
//!   every boundary case — only the *shape* (duplicate/drop to hit a
//!   constant grid) is shared.
//! * `vfr` is [`VfrDedup`], new here: forwards every frame whose
//!   presentation timestamp differs from the last one emitted, drops one
//!   that does not. This is a direct transcription of the Stage IV table's
//!   own one-line rule and needs no borrowed state machine.
//! * `auto` resolves to `cfr` or `vfr` per [`muxer_wants_cfr`] and then
//!   defers to one of the two cases above.
//!
//! # What is not implemented
//!
//! `-frame_drop_threshold` remains refused (`cli.rs`'s
//! `refuse_unimplemented_options`): it tunes exactly the reference's own
//! `do_video_out` drop decision — "how far *behind* schedule a frame may be
//! before `cfr`/`vfr` drops it rather than duplicating/emitting it" — which
//! is not a parameter either [`vaco_filter_video_format::fps::Filter`] or
//! [`VfrDedup`] takes. Accepting the option and silently not consuming it
//! would repeat exactly the defect `-ar` had before this codebase's own
//! `refuse_unimplemented_options` existed (`planning/AGENT-CONSTRAINTS.md`'s
//! standing rule: silently wrong is worse than refusing) — better to keep
//! refusing it than to accept a number that changes nothing.

use vaco_codec_core::VideoParameters;
use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::sched::Graph;
use vaco_filter_core::{Filter as CoreFilter, FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_frame::Frame;
use vaco_sched::spec::{FrameTap, PipelineSpec, SourceBind};

use crate::exit::{AvError, Diagnostic};

/// `-fps_mode`'s value, already validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpsMode {
    Passthrough,
    Cfr,
    Vfr,
    Auto,
}

impl FpsMode {
    /// Parse `-fps_mode`'s argument. See the module doc for why this is a
    /// four-way, not five-way, match.
    ///
    /// # Errors
    /// A message naming the bad value, in the reference's own wording.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "passthrough" => Ok(Self::Passthrough),
            "cfr" => Ok(Self::Cfr),
            "vfr" => Ok(Self::Vfr),
            "auto" => Ok(Self::Auto),
            other => Err(format!("Invalid value {other} specified for fps_mode")),
        }
    }
}

/// Whether the muxer this stream is going into wants a constant frame rate —
/// `auto`'s own decision rule.
///
/// No muxer in this registry currently declares such a preference (there is
/// no `MuxerFlags` bit for it yet), so this always answers `false` today,
/// which resolves every `auto` to `vfr` — the same "forward, drop only an
/// exact collision" behaviour a muxer with no opinion should get. Named as
/// its own function, not inlined, so the day a muxer gains a real
/// constant-rate requirement this is the one place that needs to change.
#[must_use]
pub const fn muxer_wants_cfr() -> bool {
    false
}

/// Insert whatever `-fps_mode` requires between `frames` and the encoder, or
/// return `frames` unchanged for `passthrough` (see the module doc for why
/// that needs no pipeline change).
///
/// # Errors
/// A [`Diagnostic`] if the graph this builds fails to configure or attach —
/// should not happen for the fixed, internally-built one-node graphs this
/// constructs, but a `Result` rather than a panic because a filter-crate
/// version skew is not something to unwrap over.
pub fn insert(
    spec: &mut PipelineSpec,
    frames: FrameTap,
    time_base: Rational,
    video: &VideoParameters,
    mode: FpsMode,
) -> Result<FrameTap, Diagnostic> {
    let effective = match mode {
        FpsMode::Passthrough => return Ok(frames),
        FpsMode::Auto => {
            if muxer_wants_cfr() {
                FpsMode::Cfr
            } else {
                FpsMode::Vfr
            }
        }
        other => other,
    };
    match effective {
        FpsMode::Cfr => insert_cfr(spec, frames, time_base, video),
        FpsMode::Vfr => insert_vfr(spec, frames, time_base, video),
        FpsMode::Passthrough | FpsMode::Auto => unreachable!("resolved above"),
    }
}

fn video_format(video: &VideoParameters, time_base: Rational) -> vaco_filter_core::LinkFormat {
    crate::filtergraph::video_link(video, time_base)
}

fn insert_cfr(
    spec: &mut PipelineSpec,
    frames: FrameTap,
    time_base: Rational,
    video: &VideoParameters,
) -> Result<FrameTap, Diagnostic> {
    let rate = if video.frame_rate.num > 0 {
        video.frame_rate
    } else {
        Rational::new(25, 1)
    };
    let filter = vaco_filter_video_format::fps::Filter::from_rate(rate)
        .map_err(|e| fps_mode_err(&e))?;
    attach_one_node(
        spec,
        frames,
        time_base,
        video,
        Box::new(Simple::new(filter)),
        "vaco_fps_mode_cfr",
    )
}

fn insert_vfr(
    spec: &mut PipelineSpec,
    frames: FrameTap,
    time_base: Rational,
    video: &VideoParameters,
) -> Result<FrameTap, Diagnostic> {
    attach_one_node(
        spec,
        frames,
        time_base,
        video,
        Box::new(Simple::new(VfrDedup::default())),
        "vaco_fps_mode_vfr",
    )
}

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// Build a one-source/one-filter/one-sink graph and attach it via
/// [`PipelineSpec::add_filter`] — the shared plumbing behind [`insert_cfr`]
/// and [`insert_vfr`].
fn attach_one_node(
    spec: &mut PipelineSpec,
    frames: FrameTap,
    time_base: Rational,
    video: &VideoParameters,
    filter: Box<dyn CoreFilter>,
    name: &'static str,
) -> Result<FrameTap, Diagnostic> {
    let format = video_format(video, time_base);
    let vaco_filter_core::LinkFormat::Video {
        format: pix_fmt, ..
    } = &format
    else {
        unreachable!("video_link always returns LinkFormat::Video")
    };
    let mut graph = Graph::new();
    let source = graph.add_source(
        "in",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(*pix_fmt)],
            label: "in".to_owned(),
            ..NodeFormats::default()
        },
    );
    let desc = FilterDesc {
        name,
        description: "internal -fps_mode node, not a real ffmpeg filter",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let node = graph.add(
        desc,
        NodeFormats::passthrough(1, 1, MediaType::Video, name),
        filter,
    );
    graph
        .connect(source, 0, node, 0)
        .map_err(|e| fps_mode_err(&e.to_string()))?;
    let sink = graph.add_sink(
        "out",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "out".to_owned(),
            ..NodeFormats::default()
        },
    );
    graph
        .connect(node, 0, sink, 0)
        .map_err(|e| fps_mode_err(&e.to_string()))?;
    graph
        .set_source_format(source, format)
        .map_err(|e| fps_mode_err(&e.to_string()))?;
    graph.configure().map_err(|e| fps_mode_err(&e.to_string()))?;
    spec.add_filter(graph, &[SourceBind::new(frames, source, time_base)], &[sink])
        .map_err(|e| fps_mode_err(&e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| fps_mode_err("a configured -fps_mode graph produced no sink tap"))
}

fn fps_mode_err(detail: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::EINVAL,
        vec![format!("-fps_mode: {detail}")],
    )
}

/// `vfr`: forward every frame whose timestamp differs from the last one
/// emitted; drop one that does not. See the module doc's Stage IV table.
#[derive(Debug, Default)]
struct VfrDedup {
    last: Option<Timestamp>,
}

impl VfrDedup {
    /// The dedup rule, independent of [`FilterContext`] so it can be
    /// exercised directly in tests — the same pattern
    /// `vaco-filter-video-format::fps::Filter::step` uses for the same
    /// reason.
    fn step(&mut self, frame: Frame) -> FrameOut {
        if self.last == Some(frame.pts) {
            FrameOut::None
        } else {
            self.last = Some(frame.pts);
            FrameOut::One(frame)
        }
    }
}

impl FrameFilter for VfrDedup {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_exactly_the_four_measured_values() {
        assert_eq!(FpsMode::parse("passthrough").unwrap(), FpsMode::Passthrough);
        assert_eq!(FpsMode::parse("cfr").unwrap(), FpsMode::Cfr);
        assert_eq!(FpsMode::parse("vfr").unwrap(), FpsMode::Vfr);
        assert_eq!(FpsMode::parse("auto").unwrap(), FpsMode::Auto);
    }

    /// Measured against the reference: `drop` is refused, not a fifth mode.
    #[test]
    fn parse_refuses_drop_and_anything_else() {
        assert!(FpsMode::parse("drop").is_err());
        assert!(FpsMode::parse("bogus").is_err());
    }

    fn frame_at(pts: i64) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool
            .acquire_video(vaco_pixfmt::PixFmt::Gray8, 2, 2)
            .unwrap();
        f.pts = Timestamp::new(pts);
        f
    }

    #[test]
    fn vfr_dedup_drops_only_an_exact_repeat() {
        let mut f = VfrDedup::default();
        let mut emitted = |pts: i64| -> bool { matches!(f.step(frame_at(pts)), FrameOut::One(_)) };
        assert!(emitted(0));
        assert!(!emitted(0), "an exact repeat must be dropped");
        assert!(emitted(1));
        assert!(emitted(2));
        assert!(!emitted(2));
    }
}
