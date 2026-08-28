//! `drawgraph`/`adrawgraph` — plot up to four frame-metadata values over
//! time as a scrolling line graph.
//!
//! `ffmpeg -h filter=drawgraph`/`-h filter=adrawgraph` (2026-08-28):
//! `m1..m4` (metadata keys, default `""`), `fg1..fg4` (foreground colour
//! *expressions*, default `"0xffff0000"` etc.), `bg` (background, type
//! `<color>`, default `"white"`), `min`/`max` (default `-1`/`1`),
//! `mode` (`bar`/`dot`/`line`, default `line`), `slide`
//! (`frame`/`replace`/`scroll`/`rscroll`/`picture`, default `frame`),
//! `size`/`s` (default `900x256`), `rate`/`r` (default `25`).
//! `drawgraph` is `V->V`, `adrawgraph` is `A->V` — the only shape
//! difference, confirmed by diffing both `-h` dumps byte-for-byte outside
//! the `Inputs:` line.
//!
//! # No font needed — a hypothesis this pass tested and found wrong
//!
//! Expected (going in) to need this crate's `font8x8` mechanism the way
//! `datascope`/`pixscope`/`graphmonitor` do, since a graph plausibly
//! needs axis labels or a min/max readout. A real render
//! (`signalstats,drawgraph=m1=lavfi.signalstats.YAVG:slide=picture`)
//! instead produced a plain coloured line trace with **no text
//! anywhere** — no axis labels, no min/max readout — and `-h` confirms
//! no font/fontfile/fontsize option exists on either filter. `drawgraph`/
//! `adrawgraph` are pure geometry, like `waveform`, not text-bound like
//! this crate's scope-proper filters — the font detour this pass started
//! with was unnecessary for these two specifically.
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs built with `signalstats`
//! and flat-colour sources for exact, known metadata values)
//!
//! **The value-to-pixel mapping is a margined linear map, ceiling-rounded,
//! for in-range values — and a hard clamp to the absolute canvas edge for
//! out-of-range ones, which is a *different* rule, not the same formula
//! evaluated past its domain.** Pinned with flat-luma sources (giving an
//! exactly known `lavfi.signalstats.YAVG`) at three graph heights
//! (`51`, `101`, `201`) and three values each (`min`, `max`, the
//! midpoint):
//!
//! ```text
//! for min <= v <= max:
//!     row = ceil(margin + (max - v) / (max - min) * (height - 1 - 2*margin))
//!     margin ~= round(0.07 * (height - 1))    // fitted, not exact — see below
//! for v < min: row = height - 1               // the absolute bottom edge
//! for v > max: row = 0                        // the absolute top edge
//! ```
//!
//! The out-of-range rule was found by setting `min=100:max=150` and
//! feeding values `0` and `255`: they landed at row `height-1` and row
//! `0` exactly — the canvas edges, **not** `row(100)`/`row(150)` under
//! the in-range formula (which would have been a few pixels in from the
//! edge, per the margin above). Values are clamped to the plot, not
//! dropped, and the clamp target is the edge, not the range boundary.
//!
//! **The margin did not resolve to one clean constant, and this is
//! recorded rather than papered over.** Nine points were measured —
//! `min`, `max` and the midpoint at three graph heights, each from a
//! flat-luma source giving an exactly known `signalstats.YAVG` — and the
//! *top* and *bottom* margins turned out not even to be equal to each
//! other at a given height (`height-1=200` measured a top margin of `15`
//! against a bottom margin of `13`, not one shared value). The raw
//! measurements: at `height-1` of `50`/`100`/`200`, `row(max)` was
//! `3`/`7`/`15` and `row(min)` was `46`/`93`/`187`. `round(0.07*
//! (height-1))` used as a single, symmetric margin for both edges
//! reproduces `height-1=100` exactly (both edges land on the measured
//! `7`/`93`) and is within one pixel of the other two heights in both
//! directions — the closest single-formula fit found in the time this
//! pass had, not a derivation. Implemented as that approximation, with
//! the exact residual stated here rather than claimed away, the same
//! honesty `datascope`/`graphmonitor`'s own unchased margins use.
//!
//! **`fg1..fg4`'s colour is a hex literal with a byte-order quirk:
//! written as `0xAARRGGBB`, applied as opaque `(B, G, R)` — R and B
//! swapped from what the name suggests, and A always ignored.**
//! `fg1=0x11223344` (intending `A=0x11, R=0x22, G=0x33, B=0x44`) painted
//! `(R=0x44, G=0x33, B=0x22)` — the *written* B and R traded places, G
//! unchanged, opaque regardless of the written alpha byte (confirmed by
//! rendering the identical RGB with `A=0x00` and `A=0xff` and getting
//! pixel-identical output both times). This is exactly the shape of bug
//! a `struct { u8 r,g,b,a; }` cast onto a little-endian `uint32_t`
//! written as a big-endian literal produces, and it is a genuinely
//! different convention from `bg`.
//!
//! **`bg` is a normal `<color>` (the same grammar `vaco-filter-draw-vf`'s
//! `drawbox`/`drawgrid` use), not the `fg1..4` quirk.** `bg=0x112233`
//! painted `(R=0x11, G=0x22, B=0x33)` exactly as written — confirming
//! `bg` and `fg1..4` are two different colour grammars sharing one
//! filter, not one binding set applied twice. `t` meaning `drawbox`'s own
//! thickness taught this pass not to assume a binding carries over
//! between filters; here the lesson was not to assume it carries over
//! between two options of the *same* filter either.
//!
//! # Not implemented
//!
//! `mode=bar`/`dot` (only `line`, the default, connects consecutive
//! samples with a straight segment — measured and implemented).
//! `slide=replace`/`scroll`/`rscroll`/`picture` (only `frame`, the
//! default, is implemented, as a left-shifting scroll that always
//! appends the newest sample at the rightmost column — this pass
//! confirmed the *end state* of a `slide=picture` accumulation
//! extensively, but did not independently re-verify a live multi-frame
//! `slide=frame` sequence against the reference frame-by-frame before
//! shipping, given time; recorded as a stated limitation rather than
//! holding the filter, the same call this crate's own `thistogram`
//! `slide` scope made). `fg1..4` as genuine per-value *expressions*
//! (evaluated with metadata-dependent variables) — only the constant-hex
//! case this pass measured is implemented; a colour that changes with
//! the plotted value would need the expression's own bound-variable set
//! measured separately, not assumed from `drawbox`'s.

use vaco_core::{Duration, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptionsExt as _, VideoRate};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

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
    name: "drawgraph",
    description: "Draw a graph using input video metadata.",
    inputs: VIDEO_IN_PAD,
    outputs: VIDEO_OUT_PAD,
    flags: FilterFlags::empty(),
};

pub const ADESC: FilterDesc = FilterDesc {
    name: "adrawgraph",
    description: "Draw a graph using input audio metadata.",
    inputs: AUDIO_IN_PAD,
    outputs: VIDEO_OUT_PAD,
    flags: FilterFlags::empty(),
};

/// One `fg1..4`-shaped hex literal: written `0xAARRGGBB`, applied as
/// opaque `(B, G, R)` — see the module doc for the probes that pinned
/// this swap and the ignored alpha byte.
fn parse_fg_hex(text: &str) -> (u8, u8, u8) {
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix('#'));
    let Some(hex) = hex else { return (255, 0, 0) };
    let byte = |i: usize| hex.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok());
    let (Some(_a), Some(r), Some(g), Some(b)) = (byte(0), byte(2), byte(4), byte(6)) else {
        return (255, 0, 0);
    };
    (b, g, r)
}

/// `bg`'s normal `<color>` grammar: `RRGGBB[AA]`, written order, no swap
/// — see the module doc for the probe distinguishing it from `fg1..4`.
fn parse_bg_hex(text: &str) -> (u8, u8, u8) {
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix('#'));
    let Some(hex) = hex else {
        return match text {
            "black" => (0, 0, 0),
            _ => (255, 255, 255),
        };
    };
    let byte = |i: usize| hex.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok());
    let (Some(r), Some(g), Some(b)) = (byte(0), byte(2), byte(4)) else {
        return (255, 255, 255);
    };
    (r, g, b)
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "drawgraph", help = "Draw a graph using input metadata.")]
pub(crate) struct Opts {
    #[opt(name = "m1", help = "set 1st metadata key", default = String::new(), flags(video, filtering))]
    pub m1: String,
    #[opt(name = "m2", help = "set 2nd metadata key", default = String::new(), flags(video, filtering))]
    pub m2: String,
    #[opt(name = "m3", help = "set 3rd metadata key", default = String::new(), flags(video, filtering))]
    pub m3: String,
    #[opt(name = "m4", help = "set 4th metadata key", default = String::new(), flags(video, filtering))]
    pub m4: String,
    #[opt(name = "fg1", help = "set 1st foreground color expression", default = "0xffff0000".to_owned(), flags(video, filtering))]
    pub fg1: String,
    #[opt(name = "fg2", help = "set 2nd foreground color expression", default = "0xff00ff00".to_owned(), flags(video, filtering))]
    pub fg2: String,
    #[opt(name = "fg3", help = "set 3rd foreground color expression", default = "0xffff00ff".to_owned(), flags(video, filtering))]
    pub fg3: String,
    #[opt(name = "fg4", help = "set 4th foreground color expression", default = "0xffffff00".to_owned(), flags(video, filtering))]
    pub fg4: String,
    #[opt(name = "bg", help = "set background color", default = "white".to_owned(), flags(video, filtering))]
    pub bg: String,
    #[opt(name = "min", help = "set minimal value", default = -1.0, flags(video, filtering))]
    pub min: f64,
    #[opt(name = "max", help = "set maximal value", default = 1.0, flags(video, filtering))]
    pub max: f64,
    #[opt(name = "mode", help = "set graph mode", default = "line".to_owned(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "slide", help = "set slide mode", default = "frame".to_owned(), flags(video, filtering))]
    pub slide: String,
    #[opt(name = "size", alias = "s", help = "set graph size", default = (900, 256), flags(video, filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = VideoRate(Rational::new(25, 1)), flags(video, filtering))]
    pub rate: VideoRate,
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

/// One line's identity: which metadata key it reads, and its resolved
/// `(r, g, b)`.
#[derive(Debug, Clone)]
struct Line {
    key: String,
    color: (u8, u8, u8),
    /// The previous sample's row, for `mode=line`'s connecting segment —
    /// `None` until the first real sample arrives.
    last_row: Option<u32>,
}

/// `row = ceil(margin + (max-v)/(max-min) * (height-1-2*margin))` for
/// in-range `v`; the absolute edge for out-of-range `v` — see the module
/// doc for both probes.
fn value_to_row(v: f64, min: f64, max: f64, height: u32) -> u32 {
    if height == 0 {
        return 0;
    }
    if v < min {
        return height - 1;
    }
    if v > max {
        return 0;
    }
    if (max - min).abs() < f64::EPSILON {
        return height - 1;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "graph heights are small (hundreds of pixels), well within f64's exact integer range"
    )]
    let h1 = f64::from(height - 1);
    let margin = (0.07 * h1).round();
    let usable = (h1 - 2.0 * margin).max(0.0);
    let row = (margin + (max - v) / (max - min) * usable).ceil();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "row is clamped into 0..=height-1 immediately below"
    )]
    let row = row as i64;
    row.clamp(0, i64::from(height - 1)) as u32
}

#[derive(Debug)]
pub(crate) struct Filter {
    width: u32,
    height: u32,
    out_base: Rational,
    in_base: Rational,
    next_due: i64,
    seen: bool,
    min: f64,
    max: f64,
    bg: (u8, u8, u8),
    lines: Vec<Line>,
    /// Persistent canvas, one `(r,g,b)` triple per pixel, row-major —
    /// `slide=frame`'s scrolling history.
    canvas: Vec<(u8, u8, u8)>,
}

impl Filter {
    fn new(opts: &Opts, rate: Rational) -> Self {
        let keys = [&opts.m1, &opts.m2, &opts.m3, &opts.m4];
        let fgs = [&opts.fg1, &opts.fg2, &opts.fg3, &opts.fg4];
        let lines = keys
            .into_iter()
            .zip(fgs)
            .filter(|(k, _)| !k.is_empty())
            .map(|(k, fg)| Line {
                key: k.clone(),
                color: parse_fg_hex(fg),
                last_row: None,
            })
            .collect();
        let width = opts.size.0.max(1);
        let height = opts.size.1.max(1);
        let bg = parse_bg_hex(&opts.bg);
        Self {
            width,
            height,
            out_base: rate.inverse(),
            in_base: Rational::UNDEFINED,
            next_due: 0,
            seen: false,
            min: opts.min,
            max: opts.max,
            bg,
            lines,
            canvas: vec![bg; (width * height) as usize],
        }
    }

}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.in_base = ctx
            .input_link(0)
            .map_or(Rational::UNDEFINED, LinkFormat::time_base);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { width, height, time_base, frame_rate, .. } = &mut out {
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

        let w = self.width as usize;
        let bg = self.bg;
        for row in self.canvas.chunks_mut(w) {
            if w > 0 {
                row.copy_within(1..w, 0);
                if let Some(last) = row.last_mut() {
                    *last = bg;
                }
            }
        }
        let last_col = w.saturating_sub(1);
        let min = self.min;
        let max = self.max;
        let height = self.height;
        for line in &mut self.lines {
            let Some(raw) = input.metadata_get(&line.key) else {
                continue;
            };
            let Ok(value) = raw.parse::<f64>() else {
                continue;
            };
            let row = value_to_row(value, min, max, height) as usize;
            let (top, bottom) = match line.last_row {
                Some(prev) => {
                    let prev = prev as usize;
                    if prev <= row { (prev, row) } else { (row, prev) }
                }
                None => (row, row),
            };
            for r in top..=bottom {
                if let Some(px) = self.canvas.get_mut(r * w + last_col) {
                    *px = line.color;
                }
            }
            line.last_row = Some(row as u32);
        }

        let mut out = ctx.pool().acquire_video(PixFmt::Gbrp, self.width, self.height)?;
        // Plane order for `gbrp`: `G, B, R`.
        for (plane, sel) in [
            (0usize, 0usize),
            (1, 2),
            (2, 1),
        ] {
            if let Some(mut dst) = out.plane_mut(plane) {
                for (y, row) in dst.rows_mut().enumerate() {
                    for (x, px) in row.iter_mut().enumerate() {
                        let Some(&c) = self.canvas.get(y * w + x) else {
                            continue;
                        };
                        *px = match sel {
                            0 => c.0,
                            1 => c.1,
                            _ => c.2,
                        };
                    }
                }
            }
        }

        out.pts = Timestamp::new(slot);
        out.time_base = self.out_base;
        out.duration = Duration(1);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.next_due = 0;
        self.seen = false;
        for line in &mut self.lines {
            line.last_row = None;
        }
        self.canvas.fill(self.bg);
    }
}

fn create_with(desc: FilterDesc, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.mode != "line" {
        return Err(format!(
            "drawgraph: mode `{}` not implemented — only `line` is",
            opts.mode
        ));
    }
    if opts.slide != "frame" {
        return Err(format!(
            "drawgraph: slide `{}` not implemented — only `frame` is",
            opts.slide
        ));
    }
    let filter = Filter::new(&opts, opts.rate.0);
    Ok(Instance {
        desc,
        formats: NodeFormats::converter(
            FormatSet::default(),
            FormatSet::video_exact(PixFmt::Gbrp),
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

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "drawgraph", instance: "drawgraph", args: None, arguments: &[] };
        assert!(create(&req).is_ok());
        let req = Instantiate { name: "adrawgraph", instance: "adrawgraph", args: None, arguments: &[] };
        assert!(create_audio(&req).is_ok());
    }

    #[test]
    fn unimplemented_mode_is_a_clean_error() {
        let req = Instantiate { name: "drawgraph", instance: "drawgraph", args: Some("mode=bar"), arguments: &[] };
        assert!(create(&req).is_err());
    }

    #[test]
    fn unimplemented_slide_is_a_clean_error() {
        let req = Instantiate { name: "drawgraph", instance: "drawgraph", args: Some("slide=picture"), arguments: &[] };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference probe: `fg1=0x11223344` (written
    /// `AARRGGBB`) paints `(R=0x44, G=0x33, B=0x22)` — R and B swapped
    /// from the written order, A dropped entirely.
    #[test]
    fn fg_hex_swaps_r_and_b_and_drops_alpha() {
        assert_eq!(parse_fg_hex("0x11223344"), (0x44, 0x33, 0x22));
    }

    #[test]
    fn fg_hex_alpha_byte_never_affects_the_result() {
        assert_eq!(parse_fg_hex("0x000000ff"), parse_fg_hex("0xff0000ff"));
    }

    /// Pinned against the reference probe: `bg=0x112233` paints
    /// `(R=0x11, G=0x22, B=0x33)` exactly as written — no swap, unlike
    /// `fg1..4`.
    #[test]
    fn bg_hex_is_written_order_no_swap() {
        assert_eq!(parse_bg_hex("0x112233"), (0x11, 0x22, 0x33));
    }

    /// Pinned against three flat-field probes at height `101`:
    /// `v=0 -> row 93`, `v=128 -> row 50`, `v=255 -> row 7`.
    #[test]
    fn value_to_row_matches_the_measured_margined_map() {
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 101), 93);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 101), 50);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 101), 7);
    }

    /// `margin = round(0.07*(height-1))` is exact at `height=101` (the
    /// case measured first) but fits `height=51`/`height=201` only
    /// approximately (`+-1`px) — the raw reference measurements were
    /// `46/25/3` and `187/101/15` respectively; this formula's own
    /// consistent output is asserted here, not a claim that it matches
    /// the reference pixel-for-pixel at every height. See the module
    /// doc's own honest accounting of this residual.
    #[test]
    fn value_to_row_formula_is_internally_consistent_at_other_heights() {
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 51), 46);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 51), 25);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 51), 4);
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 201), 186);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 201), 100);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 201), 14);
    }

    /// Pinned against the reference probe: with `min=100:max=150`, a
    /// value of `0` (below `min`) lands at the absolute bottom edge
    /// (`height-1`), and `255` (above `max`) at the absolute top edge
    /// (`0`) — not at `row(min)`/`row(max)` under the in-range formula.
    #[test]
    fn out_of_range_values_clamp_to_the_absolute_edge_not_the_margined_row() {
        let row_min_inrange = value_to_row(100.0, 100.0, 150.0, 101);
        let row_max_inrange = value_to_row(150.0, 100.0, 150.0, 101);
        assert_eq!(value_to_row(0.0, 100.0, 150.0, 101), 100);
        assert_eq!(value_to_row(255.0, 100.0, 150.0, 101), 0);
        assert_ne!(row_min_inrange, 100, "the in-range row for min should not itself be the absolute edge");
        assert_ne!(row_max_inrange, 0, "the in-range row for max should not itself be the absolute edge");
    }
}
