//! `graphmonitor`/`agraphmonitor` — draw the live filtergraph's own node and
//! link state as text. The real consumer that settles gap 22
//! (`planning/INTERFACE-GAPS.md`): that gap closed `FilterContext::
//! graph_nodes`/`graph_links`, and this module is what finds out whether
//! they actually serve the two filters that asked for them, rather than
//! merely existing.
//!
//! `ffmpeg -h filter=graphmonitor`/`-h filter=agraphmonitor` (2026-08-28):
//! identical option tables (`size`/`s` default `hd720`, `opacity`/`o`
//! `0..=1` default `0.9`, `mode`/`m` flags `full`/`compact`/`nozero`/
//! `noeof`/`nodisabled` default `0`, `flags`/`f` flags `none`/`all`/`queue`/
//! `frame_count_in`/`frame_count_out`/`frame_count_delta`/`pts`/
//! `pts_delta`/`time`/`time_delta`/`timebase`/`format`/`size`/`rate`/`eof`/
//! `sample_count_in`/`sample_count_out`/`sample_count_delta`/`disabled`
//! default `all+queue`, `rate`/`r` default `25`). `graphmonitor` is
//! `V->V`, `agraphmonitor` is `A->V` — the only shape difference is the
//! input pad's media type; both draw the same picture the same way, so one
//! `Filter` implementation serves both, matching how this crate already
//! shares font/plane helpers across filters rather than duplicating them.
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs, `-bitexact`, pixel-dumped)
//!
//! Built from `ffmpeg -f lavfi -i testsrc=... -filter_complex
//! "[0:v]split=2[a][b];[a]scale=16:16[a2];[b]scale=16:16[b2];
//! [a2][b2]hstack=inputs=2,graphmonitor=s=WxH:rate=1[out]" -map [out]
//! -pix_fmt rgb24 -f rawvideo`, pixel-dumped and cropped to individual
//! text lines.
//!
//! **Output is always `rgb24`** at exactly the `size` option's dimensions,
//! independent of the input's own geometry — a converter, not a
//! passthrough, the same shape as this crate's `histogram`.
//!
//! **The canvas is redrawn from scratch every rendered frame** (same rule
//! as `datascope`): it does not accumulate marks across frames.
//!
//! **Output is rate-gated, not one-frame-in-one-frame-out.** A `10fps`
//! video source through `graphmonitor=rate=2` for `2s` produced exactly
//! `5` output frames, not `20` — confirmed independently for
//! `agraphmonitor` (a `48kHz` sine through `agraphmonitor=rate=2` for `2s`
//! also produced `5`). Every input frame updates the filter's view of the
//! graph, but a picture is only emitted when the input's own presentation
//! time crosses the next `1/rate`-spaced slot — the same "decimate to
//! slots, don't resample into them" idea this crate's sibling crate
//! `vaco-filter-core::mock::Fps` already implements for straight video
//! rate conversion, reused here via [`vaco_core::Timestamp::rescale`].
//!
//! **Layout is one block per graph node, self included** (the monitor's
//! own node appears in its own picture): a header line
//! `"{label} {filter_name}"` flush at the left margin, one line per pad
//! after it — every input pad before every output pad, each showing the
//! label of whatever is connected to it and a live counter. **The picture
//! lists every node the graph scheduler knows about**, including nodes
//! `vaco-filter-graph`/the scheduler insert automatically (buffer sources,
//! buffersinks, auto-converters) — confirmed from a real render showing
//! `"graph 0 input from stream 0:0  buffer"`, `"out_#0:0  buffersink"` and
//! an auto-inserted `"format"` node alongside the user-named filters.
//!
//! **Row pitch is not one constant** — measured by cropping row-by-row ink
//! extents across an 8-block, 24-line render and finding the same three
//! numbers repeat exactly, block after block: `10`px from a header to its
//! first pad line and between consecutive pad lines *of the same
//! direction* (input-to-input or output-to-output); `12`px on the one
//! transition from the last input line to the first output line; `15`px
//! from a block's last line to the next block's header. This is
//! implemented exactly (see the `PITCH_*` constants) — unlike `datascope`'s
//! own margin arithmetic, this one *was* chased to the pixel, because
//! unlike a fixed value grid this filter's line count varies with the
//! graph, so an approximate pitch would visibly drift block to block.
//!
//! **Text is left-flush, variable width — not a fixed grid.** Pad lines
//! are indented by exactly one glyph cell (`8`px) from the header; peer
//! labels and counters are plain concatenated ASCII with no column
//! alignment, confirmed by measuring `'P'`'s ink starting at column `0`
//! for a header versus a sub-line's `'i'` (of `"in0:"`) starting mid-cell
//! at column `11`, consistent with one `8`px indent cell before the text
//! begins — this crate's `datascope` fixed-pitch cell grid does not apply
//! here.
//!
//! **The font is the same family**: crisp, non-antialiased, on an exact
//! `8`px horizontal pitch, matching `crate::font8x8`'s own signature — see
//! that module's doc for why it is Unscii, not the reference's own table,
//! and the same permanent, by-design text ceiling `datascope` already
//! documents applies here identically: no frame this module draws text
//! into can ever be framecrc-identical to the reference's.
//!
//! # Deliberately not implemented, and why
//!
//! This is the direct answer to whether `NodeView`/`LinkView`
//! (`vaco-filter-core`) serve these two filters unchanged: **mostly, but
//! not entirely.** Two different reasons, kept separate rather than one
//! blanket "not implemented" list:
//!
//! **Cannot be implemented against the current `NodeView`/`LinkView`
//! surface, because the data genuinely is not there** — a real finding
//! about gap 22's own scope, not a time-boxing choice: the `format` flag
//! (`LinkView` deliberately carries no pixel/sample format, colour space,
//! or geometry — gap 22's own design excluded it, see
//! `vaco-filter-core`'s `context.rs` doc); `size`/`rate`/`timebase` (same
//! reason — link geometry and timing are not part of the read-only
//! snapshot); `pts`/`pts_delta`/`time`/`time_delta` (`LinkStats` counts
//! frames and samples but does not record the *value* of the last
//! timestamp seen on a link — there is nothing to read even if a filter
//! were allowed to); `disabled` (`enable=` timeline-gating state lives on
//! the filter instance, not exposed by `NodeView`). Closing gap 22 further
//! for these would mean widening the snapshot past "the deadlock
//! diagnostic's own counters", which is exactly the boundary that gap's
//! own design note draws — a candidate for a follow-up gap if a future
//! filter needs one of these specifically, not assumed here.
//!
//! **Available on `LinkView` but not drawn, a scope choice rather than a
//! capability gap**: `frame_count_in`/`out`/`delta` and `sample_count_in`/
//! `out`/`delta` as the reference defines them are a *pair* of counters
//! (frames arrived at this link's source side versus consumed at its
//! destination side); `LinkStats.frames`/`.samples` is a single
//! post-dequeue counter, so the reference's exact three-field shape is not
//! reproducible without also tracking a push-side count this crate does
//! not currently keep — recorded rather than approximated with a
//! single number relabelled three ways. What this module draws instead,
//! using every other field `LinkView`/`LinkStats` does carry
//! (queue depth/capacity, `at_eof`, the one-sided frame/sample count, peak
//! depth, and the backpressure-blocked count): more of gap 22's own
//! surface than the reference's *default* `flags` selection shows, because
//! no rendering here can match the reference byte-for-byte regardless, so
//! showing what the capability actually offers is more useful than
//! mimicking one specific default subset of it.
//!
//! `mode=compact`/`nozero`/`noeof`/`nodisabled` (only the default,
//! `full`-shaped per-node listing is implemented); non-default `opacity`
//! (the canvas is solid black under the text, same as `datascope`'s own
//! unexplained `opacity`); non-default `flags` (this module always draws
//! its own fixed field set regardless of which flags are named — see
//! "Available on `LinkView` but not drawn" above for what that set is and
//! why). All three used to parse fine at any value and run identically to
//! the default — accepted, silently ignored, no error; `create` now
//! rejects a value that actually differs from the field's own default by
//! name instead (restating the exact default is indistinguishable from
//! never mentioning the option, so it harmlessly still creates). Colour
//! (this module draws in `Gray8`, not the reference's `rgb24` — every
//! field this crate can show is a plain counter, none of them needs a
//! second colour to distinguish, and matching the pixel format buys
//! nothing once text already rules out a byte-exact frame) stays
//! unimplemented with no rejection, since there is no `flags`-shaped
//! option value to name.

use vaco_core::{MediaType, Rational, Result, Rounding, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{
    FilterContext, FilterDesc, FilterFlags, LinkFormat, LinkView, NodeId, NodeView, Pad,
};
use vaco_frame::Frame;
use vaco_opts::{OptionsExt as _, VideoRate};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_IN_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_IN_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];
const VIDEO_OUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "graphmonitor",
    description: "Show various filtergraph stats.",
    inputs: VIDEO_IN_PAD,
    outputs: VIDEO_OUT_PAD,
    flags: FilterFlags::empty(),
};

pub const ADESC: FilterDesc = FilterDesc {
    name: "agraphmonitor",
    description: "Show various filtergraph stats.",
    inputs: AUDIO_IN_PAD,
    outputs: VIDEO_OUT_PAD,
    flags: FilterFlags::empty(),
};

/// Left/top margin — measured as `0`: the header's own `'P'` glyph starts
/// at column `0`, row `0`, unlike `datascope`'s small margin.
const MARGIN: u32 = 0;
/// Pixels of indent before a pad line's text — one glyph cell.
const INDENT: u32 = 8;
/// Header line to its own first pad line, or between two consecutive pad
/// lines of the same pad direction.
const PITCH_SAME: u32 = 10;
/// The one transition from the last input line to the first output line.
const PITCH_DIRECTION_CHANGE: u32 = 12;
/// A block's last line to the next block's header.
const PITCH_BLOCK_GAP: u32 = 15;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "graphmonitor", help = "Show various filtergraph stats.")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set monitor size", default = (1280, 720), flags(video, filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = VideoRate(Rational::new(25, 1)), flags(video, filtering))]
    pub rate: VideoRate,
    #[opt(name = "opacity", alias = "o", help = "set video opacity", default = 0.9, range = 0.0..=1.0, flags(video, filtering))]
    pub opacity: f64,
    #[opt(name = "mode", alias = "m", help = "set mode", default = String::new(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "flags", alias = "f", help = "set flags", default = "all+queue".to_owned(), flags(video, filtering))]
    pub flags: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    width: u32,
    height: u32,
    out_base: Rational,
    in_base: Rational,
    next_due: i64,
    seen: bool,
}

impl Filter {
    fn new(width: u32, height: u32, rate: Rational) -> Self {
        Self {
            width,
            height,
            out_base: rate.inverse(),
            in_base: Rational::UNDEFINED,
            next_due: 0,
            seen: false,
        }
    }
}

/// This node's label, or `"?"` if `id` names no node this snapshot knows
/// about — should not happen for a link endpoint, but a diagram must not
/// panic on a stale id.
fn node_label(nodes: &[NodeView], id: NodeId) -> &str {
    nodes
        .iter()
        .find(|n| n.id == id)
        .map_or("?", |n| n.label.as_str())
}

/// One rendered line: its top-left pixel and its ASCII bytes.
struct Line {
    top: u32,
    left: u32,
    text: Vec<u8>,
}

/// Format one pad's line: the peer's label, then every counter
/// [`LinkView`]/`LinkStats` carries — see the module doc's "Available on
/// `LinkView` but not drawn" section for why this is more fields than the
/// reference's own default `flags` selection, not fewer.
fn pad_line(dir_pad: &str, peer: &str, link: &LinkView) -> Vec<u8> {
    let (counter_label, count) = if link.media == MediaType::Audio {
        ("samples", link.stats.samples)
    } else {
        ("frames", link.stats.frames)
    };
    format!(
        "{dir_pad}: {peer}  q={}/{} {counter_label}={count} peak={} drop={} eof={}",
        link.queued,
        link.capacity,
        link.stats.peak_depth,
        link.stats.blocked,
        u8::from(link.at_eof),
    )
    .into_bytes()
}

/// Lay out every node's block: a header line then every input pad line
/// then every output pad line, at the measured pitches — see the module
/// doc's "Row pitch is not one constant" section.
fn render(nodes: &[NodeView], links: &[LinkView]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut y = MARGIN;
    for node in nodes {
        lines.push(Line {
            top: y,
            left: MARGIN,
            text: format!("{} {}", node.label, node.filter_name).into_bytes(),
        });

        let mut inputs: Vec<&LinkView> = links.iter().filter(|l| l.dst.node == node.id).collect();
        inputs.sort_by_key(|l| l.dst.pad);
        let mut outputs: Vec<&LinkView> = links.iter().filter(|l| l.src.node == node.id).collect();
        outputs.sort_by_key(|l| l.src.pad);
        let total = inputs.len() + outputs.len();

        y += if total == 0 {
            PITCH_BLOCK_GAP
        } else {
            PITCH_SAME
        };

        let mut drawn = 0usize;
        for link in &inputs {
            let peer = node_label(nodes, link.src.node);
            let text = pad_line(&format!("in{}", link.dst.pad), peer, link);
            lines.push(Line {
                top: y,
                left: MARGIN + INDENT,
                text,
            });
            drawn += 1;
            y += if drawn == total {
                PITCH_BLOCK_GAP
            } else if drawn == inputs.len() {
                PITCH_DIRECTION_CHANGE
            } else {
                PITCH_SAME
            };
        }
        for link in &outputs {
            let peer = node_label(nodes, link.dst.node);
            let text = pad_line(&format!("out{}", link.src.pad), peer, link);
            lines.push(Line {
                top: y,
                left: MARGIN + INDENT,
                text,
            });
            drawn += 1;
            y += if drawn == total {
                PITCH_BLOCK_GAP
            } else {
                PITCH_SAME
            };
        }
    }
    lines
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.in_base = ctx
            .input_link(0)
            .map_or(Rational::UNDEFINED, LinkFormat::time_base);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.out_base;
                *frame_rate = self.out_base.inverse();
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        // Rate-gate: consume every input frame (it has already updated the
        // link counters this draws, just by having flowed through the
        // graph to reach us), but only render on the slots `rate` defines
        // — see the module doc's "Output is rate-gated" section.
        let slot = if self.in_base.is_undefined() {
            self.next_due
        } else {
            input
                .pts
                .rescale(self.in_base, self.out_base, Rounding::Down)
                .ticks()
                .unwrap_or(self.next_due)
        };
        if !self.seen {
            self.seen = true;
            self.next_due = slot;
        }
        if slot < self.next_due {
            return Ok(FrameOut::None);
        }
        self.next_due = slot.saturating_add(1);

        let mut out = ctx
            .pool()
            .acquire_video(PixFmt::Gray8, self.width, self.height)?;
        if let Some(mut plane) = out.plane_mut(0) {
            plane.fill(0);
        }

        // Snapshot first: `graph_nodes`/`graph_links` borrow `ctx`
        // immutably, and `out.plane_mut` below needs no further access to
        // `ctx` once these are collected.
        let nodes = ctx.graph_nodes().to_vec();
        let links = ctx.graph_links();
        let text_lines = render(&nodes, &links);

        if let Some(mut dst) = out.plane_mut(0) {
            let mut rows: Vec<&mut [u8]> = dst.rows_mut().collect();
            for line in &text_lines {
                common::draw_text(&mut rows, line.top, line.left, &line.text);
            }
        }

        out.pts = Timestamp::new(slot);
        out.time_base = self.out_base;
        out.set_duration_ticks(1);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.next_due = 0;
        self.seen = false;
    }
}

fn create_with(desc: FilterDesc, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    // `opacity`/`mode`/`flags` used to parse fine at any value -- including
    // the reference's own defaults -- and be silently discarded regardless:
    // this filter always draws its own fixed field set, solid black, no
    // per-node/per-link filtering, whatever those three options say. That
    // is a real gap for every value, not just non-default ones (see the
    // module doc's "Not measured/implemented"), so a value that actually
    // differs from the field's own default is rejected here -- restating
    // the exact default (indistinguishable, once parsed, from never
    // mentioning the option at all) harmlessly still creates, matching
    // `datascope`'s narrower "non-default rejected" shape as closely as
    // three fully-unimplemented options allow.
    #[allow(
        clippy::float_cmp,
        reason = "0.9 is the field's own literal default, not computed"
    )]
    if opts.opacity != 0.9 {
        return Err(format!(
            "{}: opacity is not implemented (the canvas is always solid black under the text; \
             see this module's doc)",
            desc.name
        ));
    }
    if !opts.mode.is_empty() {
        return Err(format!(
            "{}: mode is not implemented (only the default, full-shaped per-node listing is \
             drawn; see this module's doc)",
            desc.name
        ));
    }
    if opts.flags != "all+queue" {
        return Err(format!(
            "{}: flags is not implemented (this filter always draws its own fixed field set \
             regardless of flags; see this module's doc)",
            desc.name
        ));
    }
    let filter = Filter::new(opts.size.0.max(1), opts.size.1.max(1), opts.rate.0);
    Ok(Instance {
        desc,
        formats: NodeFormats::converter(
            FormatSet::default(),
            FormatSet::video_exact(PixFmt::Gray8),
            req.instance,
        ),
        filter: Box::new(Simple::new(filter)),
    })
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    create_with(DESC, req)
}

pub(crate) fn create_audio(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    create_with(ADESC, req)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_filter_core::LinkId;
    use vaco_filter_core::link::LinkStats;
    use vaco_filter_core::link::{Direction, PadRef};

    fn link(src: NodeId, src_pad: u32, dst: NodeId, dst_pad: u32, media: MediaType) -> LinkView {
        LinkView {
            id: LinkId(0),
            src: PadRef {
                node: src,
                direction: Direction::Output,
                pad: src_pad,
            },
            dst: PadRef {
                node: dst,
                direction: Direction::Input,
                pad: dst_pad,
            },
            media,
            queued: 0,
            capacity: 8,
            at_eof: false,
            stats: LinkStats::default(),
        }
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "graphmonitor",
            instance: "graphmonitor",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
        let req = Instantiate {
            name: "agraphmonitor",
            instance: "agraphmonitor",
            args: None,
            arguments: &[],
        };
        assert!(create_audio(&req).is_ok());
    }

    /// `opacity`/`mode`/`flags` used to parse fine at any value -- even
    /// the reference's own default -- and be silently discarded either
    /// way. A value that actually differs from the field's own default is
    /// now a named error (there is no way to distinguish "never
    /// mentioned" from "explicitly restated the exact default" once
    /// parsed, so restating the default harmlessly still creates -- it
    /// asks for exactly what already happens); see
    /// `creatable_with_defaults` for the never-mentioned case.
    #[test]
    fn unimplemented_options_are_a_named_error_not_a_silent_substitution() {
        for args in ["opacity=0.5", "mode=full", "flags=queue"] {
            let req = Instantiate {
                name: "graphmonitor",
                instance: "graphmonitor",
                args: Some(args),
                arguments: &[],
            };
            let err = create(&req).unwrap_err();
            assert!(
                err.contains("graphmonitor") && err.contains("not implemented"),
                "{args}: unexpected error text: {err}"
            );
        }
    }

    /// A node with one input and one output produces exactly three lines
    /// (header, `in0`, `out0`), at the measured pitches: `10` from header
    /// to `in0`, `12` from `in0` to `out0` (the one direction change).
    #[test]
    fn one_in_one_out_node_gets_header_then_input_then_output() {
        let a = NodeId(0);
        let b = NodeId(1);
        let nodes = vec![
            NodeView {
                id: a,
                label: "src".to_owned(),
                filter_name: "testsrc",
            },
            NodeView {
                id: b,
                label: "scale_0".to_owned(),
                filter_name: "scale",
            },
        ];
        let links = vec![link(a, 0, b, 0, MediaType::Video)];
        let lines = render(&nodes, &links);
        // Node `a` (no pads reference it as a destination or source in a
        // way that gives it a line beyond its own header plus its one
        // output) and node `b` (one input, no outputs) are both present.
        assert!(lines.iter().any(|l| l.text == b"src testsrc"));
        let header_b = lines.iter().find(|l| l.text == b"scale_0 scale").unwrap();
        let in0_b = lines
            .iter()
            .find(|l| l.text.starts_with(b"in0: src"))
            .unwrap();
        assert_eq!(in0_b.top - header_b.top, PITCH_SAME);
        assert_eq!(in0_b.left, MARGIN + INDENT);

        let header_a = lines.iter().find(|l| l.text == b"src testsrc").unwrap();
        let out0_a = lines
            .iter()
            .find(|l| l.text.starts_with(b"out0: scale_0"))
            .unwrap();
        assert_eq!(out0_a.top - header_a.top, PITCH_SAME);
    }

    /// A node with both an input and an output pays the measured
    /// direction-change pitch (`12`, not `10`) exactly once, on the
    /// transition from its last input line to its first output line.
    #[test]
    fn direction_change_pitch_applies_between_input_and_output_groups() {
        let a = NodeId(0);
        let b = NodeId(1);
        let c = NodeId(2);
        let nodes = vec![
            NodeView {
                id: a,
                label: "src".to_owned(),
                filter_name: "testsrc",
            },
            NodeView {
                id: b,
                label: "mid".to_owned(),
                filter_name: "scale",
            },
            NodeView {
                id: c,
                label: "sink".to_owned(),
                filter_name: "nullsink",
            },
        ];
        let links = vec![
            link(a, 0, b, 0, MediaType::Video),
            link(b, 0, c, 0, MediaType::Video),
        ];
        let lines = render(&nodes, &links);
        // `mid`'s own in0 (from `src`) and out0 (to `sink`) — named by peer
        // to avoid matching `sink`'s own in0 or `src`'s own out0, which
        // also start with the same prefixes.
        let in0 = lines
            .iter()
            .find(|l| l.text.starts_with(b"in0: src"))
            .unwrap();
        let out0 = lines
            .iter()
            .find(|l| l.text.starts_with(b"out0: sink"))
            .unwrap();
        assert_eq!(out0.top - in0.top, PITCH_DIRECTION_CHANGE);
    }

    /// The gap after a block's last line, before the next block's header,
    /// is the measured `15`, not the same-direction `10`.
    #[test]
    fn block_gap_separates_consecutive_node_headers() {
        let a = NodeId(0);
        let b = NodeId(1);
        let nodes = vec![
            NodeView {
                id: a,
                label: "one".to_owned(),
                filter_name: "f",
            },
            NodeView {
                id: b,
                label: "two".to_owned(),
                filter_name: "g",
            },
        ];
        let links = vec![link(a, 0, b, 0, MediaType::Video)];
        let lines = render(&nodes, &links);
        let out0 = lines.iter().find(|l| l.text.starts_with(b"out0:")).unwrap();
        let header_two = lines.iter().find(|l| l.text == b"two g").unwrap();
        assert_eq!(header_two.top - out0.top, PITCH_BLOCK_GAP);
    }

    /// A node with no pads at all still gets its header line, followed by
    /// the block gap (there is no pad-line pitch to fall back on).
    #[test]
    fn a_pad_less_node_still_gets_a_header_and_the_block_gap() {
        let a = NodeId(0);
        let b = NodeId(1);
        let nodes = vec![
            NodeView {
                id: a,
                label: "isolated".to_owned(),
                filter_name: "f",
            },
            NodeView {
                id: b,
                label: "next".to_owned(),
                filter_name: "g",
            },
        ];
        let lines = render(&nodes, &[]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].top - lines[0].top, PITCH_BLOCK_GAP);
    }

    #[test]
    fn an_unknown_peer_id_reports_a_placeholder_rather_than_panicking() {
        assert_eq!(node_label(&[], NodeId(9)), "?");
    }
}
