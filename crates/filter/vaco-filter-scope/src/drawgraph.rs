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
//! # Correction, 2026-08-28: the previous pass's "margined" mapping was a
//! measurement artifact, not a real feature
//!
//! A later dispatch asked for one more focused probe into an apparent
//! margin asymmetry (`height-1=200` had measured a top margin of `15`
//! against a bottom margin of `13`). Re-measuring with a cleaner method —
//! `slide=picture`, a value held **constant** across several frames (so
//! `mode=line`'s connecting segment is a flat run at the true row, not a
//! diagonal between two different values), and reading column `0` (the
//! very first plotted point, never touched by any scroll/reset logic) —
//! found **no margin at all**, at every one of 77 points checked (11
//! heights from `51` to `250`, 7 values from `0` to `255` each):
//!
//! ```text
//! row = clamp(floor((max - v) / (max - min) * (height - 1)), 0, height - 1)
//! ```
//!
//! This single formula, with no special case, also explains the
//! previously-reported "out-of-range clamps to the absolute edge, a
//! *different* rule" finding: feeding `v=0`/`v=255` against `min=100:
//! max=150` makes the unclamped quotient `3.0`/`-2.1`, wildly outside
//! `0..=height-1` regardless of margin, so *any* formula's natural
//! `.clamp()` would land on the same edges that probe saw. That probe
//! never actually distinguished a separate out-of-range rule from an
//! in-range formula's own overflow — both this formula and the old
//! margined one clamp to the same place for values that far outside
//! range, so it could not have told them apart. The real error was
//! upstream of that probe: the original margined formula was fitted to
//! numbers gathered under `slide=frame`, and (see below) `slide=frame`
//! turned out not to behave as this pass assumed, so the columns being
//! read did not contain what they were assumed to contain. Once measured
//! with a method that removes that confound, the margin disappears
//! entirely and the fit was fitting noise. This crate's own `pixscope`
//! and `graphmonitor` margins are still open — they were not re-measured
//! by this correction — but this is a reminder that a "fitted, not
//! exact" residual is itself a flag to re-probe with a cleaner method
//! before shipping it, not a stable resting point.
//!
//! # Correction, 2026-08-28: `slide=frame` does not scroll — it fills
//! left-to-right and wipes on overflow
//!
//! The same re-probe (a distinct, monotonically increasing value fed to
//! one column per emitted frame, `width=10`) found `slide=frame` (mode
//! `0`, the default — the very mode already shipped) draws each new
//! sample into the **next unfilled column**, starting at column `0`,
//! not a persistent buffer that shifts left and always appends at the
//! rightmost column (which is what this pass had implemented, and what
//! `slide=scroll`'s own name and description — "scroll from right to
//! left" — actually names). Once every column has a value, the *next*
//! sample clears the whole canvas back to `bg` and starts again at
//! column `0` — confirmed watching the hit count drop from `10` filled
//! columns to `1` the instant the buffer filled. The per-line "vertical
//! bar from the previous row to the new row" drawing this pass already
//! had (see `filter_frame`) turned out to be right and needed no change
//! — only *which column* it draws into. `Line::last_row` state also
//! carries across a wipe unchanged (the first bar drawn into column `0`
//! after a wipe connects to the value last plotted before the wipe, at
//! the old rightmost column) — confirmed by the connecting bar's span
//! matching the pre-wipe value's row, not starting fresh. This was a
//! real bug in what was reported as shipped, found only because the
//! coordinator's own re-probe request for the margin question forced a
//! cleaner measurement method that exposed it as a side effect — the
//! margin fit and the slide confound were two symptoms of the same
//! methodological gap, not two unrelated errors.
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
//! **This is bug-for-bug compatibility, not a defect left unfixed: `vaco`
//! reproduces the swap.** `parse_fg_hex` deliberately returns the same
//! `(B, G, R)`-from-`AARRGGBB` mapping the reference applies. A
//! differential test against the reference needs `fg1=0x11223344` to
//! paint the same wrong-looking `(R=0x44, G=0x33, B=0x22)`, not the
//! "corrected" `(R=0x22, G=0x33, B=0x44)` the option's own name implies —
//! correcting it here would make every corpus entry that sets `fg1..4`
//! diverge from the reference instead of matching it. Do not "fix" this
//! swap; it is the reference's own behaviour, pinned by
//! `fg_hex_swaps_r_and_b_and_drops_alpha` below.
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
//! default, is implemented — fill-left-to-right-then-wipe, re-measured
//! and corrected above after the original scroll assumption was found
//! wrong). `fg1..4` as genuine per-value *expressions*
//! (evaluated with metadata-dependent variables) — only the constant-hex
//! case this pass measured is implemented; a colour that changes with
//! the plotted value would need the expression's own bound-variable set
//! measured separately, not assumed from `drawbox`'s.

use vaco_core::{MediaType, Rational, Result, Rounding, Timestamp};
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
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
    };
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
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
    };
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
    #[opt(
        name = "max",
        help = "set maximal value",
        default = 1.0,
        flags(video, filtering)
    )]
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

/// `row = clamp(floor((max-v)/(max-min) * (height-1)), 0, height-1)` — no
/// margin. Matched exactly at 77/77 measured points (11 heights, 7 values
/// each, including out-of-range `v`) — see the module doc's 2026-08-28
/// correction for how the previously-shipped margined formula was found
/// to be a measurement artifact.
fn value_to_row(v: f64, min: f64, max: f64, height: u32) -> u32 {
    if height == 0 {
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
    let row = ((max - v) / (max - min) * h1).floor();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "row is clamped into 0..=height-1 immediately below; NaN/inf cannot occur since max != min was ruled out above"
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
    /// Persistent canvas, one `(r,g,b)` triple per pixel, row-major.
    /// `slide=frame` does **not** scroll (that is `slide=scroll`'s own,
    /// unimplemented, behaviour) — it fills columns left-to-right from
    /// `next_col` and wipes the whole canvas back to `bg` once full, see
    /// the module doc's 2026-08-28 correction.
    canvas: Vec<(u8, u8, u8)>,
    /// The next column `slide=frame` will draw into; wraps (with a full
    /// canvas wipe) back to `0` on overflow.
    next_col: usize,
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
            next_col: 0,
        }
    }
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
        if w == 0 || self.next_col >= w {
            self.canvas.fill(self.bg);
            self.next_col = 0;
        }
        let col = self.next_col;
        self.next_col = self.next_col.saturating_add(1);
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
                    if prev <= row {
                        (prev, row)
                    } else {
                        (row, prev)
                    }
                }
                None => (row, row),
            };
            for r in top..=bottom {
                if let Some(px) = self.canvas.get_mut(r * w + col) {
                    *px = line.color;
                }
            }
            line.last_row = Some(row as u32);
        }

        let mut out = ctx
            .pool()
            .acquire_video(PixFmt::Gbrp, self.width, self.height)?;
        // Plane order for `gbrp`: `G, B, R`.
        for (plane, sel) in [(0usize, 0usize), (1, 2), (2, 1)] {
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
        out.set_duration_ticks(1);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.next_due = 0;
        self.seen = false;
        self.next_col = 0;
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
        let req = Instantiate {
            name: "drawgraph",
            instance: "drawgraph",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
        let req = Instantiate {
            name: "adrawgraph",
            instance: "adrawgraph",
            args: None,
            arguments: &[],
        };
        assert!(create_audio(&req).is_ok());
    }

    #[test]
    fn unimplemented_mode_is_a_clean_error() {
        let req = Instantiate {
            name: "drawgraph",
            instance: "drawgraph",
            args: Some("mode=bar"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn unimplemented_slide_is_a_clean_error() {
        let req = Instantiate {
            name: "drawgraph",
            instance: "drawgraph",
            args: Some("slide=picture"),
            arguments: &[],
        };
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

    /// Re-measured 2026-08-28 with `slide=picture`, a value held constant
    /// across frames, and column `0` (immune to the `slide=frame` column
    /// confound the original margined fit was actually measuring). No
    /// margin: `v=0 -> row 100` (the absolute bottom edge), `v=128 ->
    /// row 49`, `v=255 -> row 0` (the absolute top edge), at height 101.
    #[test]
    fn value_to_row_matches_the_measured_unmargined_map() {
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 101), 100);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 101), 49);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 101), 0);
    }

    /// The same unmargined formula, exact (not merely "internally
    /// consistent") against 77 independently measured points spanning
    /// heights `51` through `250` and values `0`/`64`/`100`/`128`/`150`/
    /// `191`/`255` — a sample of that sweep at three heights, replacing
    /// the old `+-1`px-residual test this correction retired.
    #[test]
    fn value_to_row_formula_is_exact_at_other_heights() {
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 51), 50);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 51), 24);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 51), 0);
        assert_eq!(value_to_row(0.0, 0.0, 255.0, 201), 200);
        assert_eq!(value_to_row(128.0, 0.0, 255.0, 201), 99);
        assert_eq!(value_to_row(255.0, 0.0, 255.0, 201), 0);
        assert_eq!(value_to_row(150.0, 0.0, 255.0, 250), 102);
    }

    /// The 2026-08-28 correction found the "out-of-range clamps to the
    /// absolute edge" finding is not a separate rule at all: the same
    /// unmargined formula's own quotient is already outside `0..=height-1`
    /// for a genuinely out-of-range value, so `.clamp()` alone produces
    /// the edge — and, per the corrected formula, an *in-range* value
    /// exactly at `min`/`max` already lands on that same edge, so there
    /// is no observable difference to test for any more. This test now
    /// pins the unified behaviour: values outside `[min, max]` clamp to
    /// the same edge their nearest in-range boundary value would.
    #[test]
    fn out_of_range_values_clamp_the_same_as_the_in_range_formula_would() {
        assert_eq!(value_to_row(100.0, 100.0, 150.0, 101), 100);
        assert_eq!(value_to_row(150.0, 100.0, 150.0, 101), 0);
        assert_eq!(value_to_row(0.0, 100.0, 150.0, 101), 100);
        assert_eq!(value_to_row(255.0, 100.0, 150.0, 101), 0);
    }
}
