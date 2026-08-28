//! `datascope` — draw each sample's raw value as text, on a fixed-size grid.
//!
//! `ffmpeg -h filter=datascope` (2026-08-28): `size`/`s` (default `hd720`),
//! `x`/`y` (source offset, default `0`), `mode` (`mono`/`color`/`color2`,
//! default `mono`), `axis` (bool, default `false`), `opacity` (`0..=1`,
//! default `0.75`), `format` (`hex`/`dec`, default `hex`), `components`
//! (bitmask `1..=15`, default `15`). No font/fontfile/fontsize option
//! exists — see `crate::font8x8` for why that, and the shape of the
//! rendered output, means this filter draws with a compiled-in bitmap
//! font rather than anything `vaco-filter-text`'s `TextRenderer` (#462)
//! would provide.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! Output size is exactly the `size` option, independent of the input's
//! own dimensions, and the *pixel format* is passed through unchanged (an
//! all-black `32x32` `gray` source through `datascope=s=64x32` still
//! reports `gray` on the output pad; a `yuv420p` source through
//! `datascope=s=32x16:mode=mono` reports `yuv420p` out, with every chroma
//! sample forced to the neutral value `128`) — a `passthrough` link, not a
//! forced-format `converter` the way this crate's `histogram` is.
//!
//! The canvas is **not** a copy or crop of the source frame: an all-white
//! source produces the exact same `0` background as an all-black one
//! everywhere outside the glyphs. Every frame starts from a fresh
//! zero-filled canvas; only the value text is drawn onto it.
//!
//! Per visible grid cell, the displayed value is the source sample at
//! `(x_option + column, y_option + row)`, raster order, confirmed three
//! ways: an all-`0`/all-`0xFF` source pins the digit shapes at value `00`
//! and `FF`; a synthetic gradient (`value = 10*x mod 256` per column,
//! constant per row) pins the *sequence* of decimal/hex values read off
//! left-to-right, in both `format=hex` and `format=dec`; and re-running
//! the same gradient with `x=2` shows the first two columns' values
//! disappear from the output, confirming `x`/`y` shift which source
//! sample a given grid cell reads rather than cropping the canvas.
//!
//! Within one cell, consecutive digits sit on an exact 8-pixel pitch
//! (`format=hex`: `FIRST` digit's glyph starts a `size`-independent
//! `20`px-pitch grid, second digit `8`px later; `format=dec`: the same
//! `8`px intra-number pitch, `30`px between numbers). This crate's own
//! font is only coincidentally `8`px wide — see `crate::font8x8`'s doc for
//! why that is not the reference's own metric and does not make text
//! output framecrc-comparable.
//!
//! # Not measured/implemented
//!
//! `mode=color`/`mode=color2` (colour-coded text per component; only
//! `mode=mono` is implemented). `axis` (row/column index labels drawn in
//! a margin). `opacity` (a background-box blend that this crate's own
//! probes could not isolate a visible effect for against a canvas that is
//! already solid black outside the glyphs — plausibly a highlight
//! reserved for `axis` mode or for use when overlaid over another
//! filter's output, neither of which this pass measured). RGB pixel
//! formats (`is_rgb()` — plane 0 is not a luma/value channel there, so
//! this filter declines rather than drawing garbage). Bit depths above 8.
//! `components` beyond the first selected plane (matches this crate's
//! `histogram` precedent: verified for the single-plane case, multi-plane
//! stacking is a documented, unverified extrapolation — here, not even
//! attempted, since no multi-plane probe was run).
//!
//! The exact inter-cell gap this module uses (a flat "one glyph pitch"
//! margin after each number, i.e. `CELL_W_HEX = 20`, `CELL_W_DEC = 30`,
//! `ROW_PITCH = 12`) matches the reference's *measured* pitches, but this
//! module did not chase the reference's own margin arithmetic (`4`px
//! after a 2-digit hex number, `6`px after a 3-digit decimal one — not
//! obviously one constant) to the last pixel, because doing so cannot
//! produce a framecrc match anyway once the glyph shapes themselves
//! differ (see `crate::font8x8`).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::font8x8::GLYPH_H;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "datascope",
    description: "Video data analysis.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Left/top margin before the first glyph, in pixels — matches the
/// reference's measured first-glyph position closely enough for a
/// structural (non-framecrc) layout; see the module doc.
const MARGIN: u32 = 2;
/// Pixels between the start of consecutive vertical grid rows.
const ROW_PITCH: u32 = GLYPH_H as u32 + 4;
/// Pixels between the start of consecutive `format=hex` (2-digit) cells.
const CELL_W_HEX: u32 = 20;
/// Pixels between the start of consecutive `format=dec` (3-digit) cells.
const CELL_W_DEC: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberFormat {
    Hex,
    Dec,
}

impl NumberFormat {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "hex" => Some(Self::Hex),
            "dec" => Some(Self::Dec),
            _ => None,
        }
    }

    const fn cell_width(self) -> u32 {
        match self {
            Self::Hex => CELL_W_HEX,
            Self::Dec => CELL_W_DEC,
        }
    }

    /// Render `v` as this format's fixed-width, zero-padded ASCII digits.
    fn digits(self, v: u8) -> Vec<u8> {
        match self {
            Self::Hex => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                let hi = HEX.get(usize::from(v >> 4)).copied().unwrap_or(b'0');
                let lo = HEX.get(usize::from(v & 0x0F)).copied().unwrap_or(b'0');
                vec![hi, lo]
            }
            Self::Dec => {
                let s = format!("{v:03}");
                s.into_bytes()
            }
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "datascope", help = "Video data analysis.")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set output size", default = (1280, 720), flags(video, filtering))]
    pub size: (u32, u32),
    #[opt(name = "x", help = "set x offset", default = 0, range = 0..=i64::MAX, flags(video, filtering))]
    pub x: i64,
    #[opt(name = "y", help = "set y offset", default = 0, range = 0..=i64::MAX, flags(video, filtering))]
    pub y: i64,
    #[opt(name = "mode", help = "set scope mode", default = "mono".to_owned(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "format", help = "set display number format", default = "hex".to_owned(), flags(video, filtering))]
    pub format: String,
    #[opt(name = "components", help = "set components to display", default = 15, range = 1..=15, flags(video, filtering))]
    pub components: i64,
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
    x: u32,
    y: u32,
    format: NumberFormat,
    /// `false` for anything other than the measured `mode=mono` — the
    /// filter still runs (rather than erroring) but draws nothing extra
    /// for the unimplemented modes beyond the plain value grid, which is
    /// the honest thing to do rather than silently mislabelling `color`
    /// output as `mono`'s.
    mono: bool,
}

/// Draw one already-formatted number (fixed-width ASCII digits) starting
/// at `(left, top)`, one glyph per digit — exactly `common::draw_text`,
/// kept as a thin named wrapper so call sites below read "draw a number"
/// rather than "draw a text".
fn draw_number(rows: &mut [&mut [u8]], top: u32, left: u32, digits: &[u8]) {
    common::draw_text(rows, top, left, digits);
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(mut out) = ctx.output_link(0).cloned() else {
            return Ok(());
        };
        if let LinkFormat::Video { width, height, .. } = &mut out {
            *width = self.width;
            *height = self.height;
        }
        ctx.set_output_link(0, out);
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() || format.is_rgb() {
            // Structural scope: plane 0 must be a luma/value channel for
            // "draw the raw sample value" to mean anything; declined for
            // formats where it is not, rather than drawing onto the
            // wrong channel. See the module doc's "Not implemented".
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { height: in_h, .. }) = ctx.input_link(0).cloned()
        else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, self.width, self.height)?;

        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let Some(mut dst) = out.plane_mut(plane) else {
                continue;
            };
            let neutral: u8 = if plane == 0 { 0 } else { 128 };
            dst.fill(neutral);
        }
        if format.has_alpha()
            && plane_count > 0
            && let Some(mut alpha) = out.plane_mut(plane_count - 1)
        {
            alpha.fill(255);
        }

        // Only `mode=mono`'s single measured behaviour is implemented: the
        // value grid on plane 0, sourced from the input's own plane 0.
        if self.mono
            && let Some(src) = input.plane(0)
        {
            let src_h = usize::try_from(format.plane_height(in_h, 0)).unwrap_or(0);
            let cell_w = self.format.cell_width();
            #[allow(
                clippy::integer_division,
                reason = "counting how many whole glyph cells fit in the canvas \
                          is an exact floor by construction, not a lossy average"
            )]
            let cols = (self.width.saturating_sub(MARGIN)) / cell_w.max(1);
            #[allow(
                clippy::integer_division,
                reason = "counting how many whole glyph rows fit in the canvas \
                          is an exact floor by construction, not a lossy average"
            )]
            let rows_count = (self.height.saturating_sub(MARGIN)) / ROW_PITCH.max(1);

            if let Some(mut dst) = out.plane_mut(0) {
                let mut rows: Vec<&mut [u8]> = dst.rows_mut().collect();
                for r in 0..rows_count {
                    let sy = self.y as usize + r as usize;
                    if sy >= src_h {
                        break;
                    }
                    let Some(src_row) = src.row(sy) else { continue };
                    for c in 0..cols {
                        let sx = self.x as usize + c as usize;
                        let Some(&v) = src_row.get(sx) else { continue };
                        let digits = self.format.digits(v);
                        let top = MARGIN + r * ROW_PITCH;
                        let left = MARGIN + c * cell_w;
                        draw_number(&mut rows, top, left, &digits);
                    }
                }
            }
        }

        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let format = NumberFormat::from_name(&opts.format)
        .ok_or_else(|| format!("datascope: bad `format` `{}`", opts.format))?;
    let mono = opts.mode == "mono";
    let filter = Filter {
        width: opts.size.0.max(1),
        height: opts.size.1.max(1),
        x: u32::try_from(opts.x).unwrap_or(0),
        y: u32::try_from(opts.y).unwrap_or(0),
        format,
        mono,
    };
    let _ = opts.components;
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

    /// Pinned against the reference probe in this module's doc: value
    /// `0` formats as `"00"` in hex, `"000"` in decimal.
    #[test]
    fn hex_and_dec_formatting_matches_the_reference_probe() {
        assert_eq!(NumberFormat::Hex.digits(0), b"00");
        assert_eq!(NumberFormat::Dec.digits(0), b"000");
    }

    /// Pinned: value `255` (an all-white source) formats as `"FF"` /
    /// `"255"` — uppercase hex, matching the probed glyph shapes.
    #[test]
    fn max_value_formatting() {
        assert_eq!(NumberFormat::Hex.digits(255), b"FF");
        assert_eq!(NumberFormat::Dec.digits(255), b"255");
    }

    /// Pinned: the gradient probe's second column (`value = 10`) reads
    /// `"0A"` in hex and `"010"` in decimal.
    #[test]
    fn gradient_second_column_formatting() {
        assert_eq!(NumberFormat::Hex.digits(10), b"0A");
        assert_eq!(NumberFormat::Dec.digits(10), b"010");
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "datascope",
            instance: "datascope",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_format_is_a_clean_error() {
        let req = Instantiate {
            name: "datascope",
            instance: "datascope",
            args: Some("format=nope"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
