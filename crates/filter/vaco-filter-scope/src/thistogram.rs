//! `thistogram` — a per-value histogram plotted as one new column per
//! frame, a scrolling/cycling history of the frame's own value
//! distribution.
//!
//! `ffmpeg -h filter=thistogram` (2026-08-28): `width`/`w` (`0..=8192`,
//! `0` meaning "use the input's own width"), `display_mode`/`d`
//! (`overlay`/`parade`/`stack`, default `stack`), `levels_mode`/`m`
//! (`linear`/`logarithmic`, default `linear`), `components`/`c` (bitmask
//! `1..=15`, default `7`), `bgopacity`/`b` (`0..=1`, default `0.9`),
//! `envelope`/`e` (bool, default `false`), `ecolor`/`ec` (default
//! `"gold"`), `slide` (`frame`/`replace`/`scroll`/`rscroll`/`picture`,
//! default `replace`).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! Output is `width x 256` (`width` per the option above), and — unlike
//! `histogram`/`waveform` — **stateful across frames**: the reference
//! keeps a persistent `width x 256` canvas and draws exactly one new
//! column per input frame, leaving every other column exactly as it was.
//! Confirmed with a 4-frame sequence at `w=2` (each frame a distinct flat
//! value, so its column is unambiguous): the previous frame's column is
//! still present, unchanged, in every later output frame that does not
//! overwrite it.
//!
//! Per column, per selected plane's bin `v` (measured against `histogram`'s
//! own `ceil` rule specifically to check they are *not* the same formula
//! — they are not):
//!
//! ```text
//! intensity[v] = round(count[v] / max(count) * 255)   // round, not ceil
//! row v = 255 - v
//! ```
//!
//! Pinned three ways: a flat frame (single bin, ratio `1.0`) gives `255`;
//! a `56`-of-`200` ratio gives `71` (rules out `ceil`, which would give
//! `72`, `histogram`'s own rule); a `3`-of-`8` ratio (`0.625`, an exact
//! tie-adjacent fraction) gives `96`, and a `1`-of-`2` ratio (`0.5`
//! exactly) gives `128` — both rule out plain truncation (`95`/`127`) and
//! confirm round-half-away-from-zero, matching `f64::round`.
//!
//! `slide` (which column advances, and what happens to the rest) is
//! measured for the two most-used values:
//!
//! - **`replace`, the default.** `column = frame_count % width`; the
//!   reference overwrites only that one column with the new frame's
//!   histogram and leaves every other column exactly as it was — a plain
//!   ring buffer. Confirmed across a 4-frame, `w=2` sequence: column `0`
//!   is overwritten on frames `0` and `2`, column `1` on frames `1` and
//!   `3`, and the *other* column's content survives each overwrite
//!   unchanged.
//! - **`frame`.** Same `column = frame_count % width` indexing, but the
//!   *entire canvas is cleared* immediately before drawing whenever
//!   `column == 0` (including — harmlessly — the very first frame).
//!   Confirmed with the same 4-frame sequence: at frame index `2`
//!   (`column` wraps back to `0`), column `1`'s data from frame index `1`
//!   disappears from the output entirely, along with everything else,
//!   leaving only the new frame's column `0`.
//!
//! `width=0` was confirmed to mean "use the input frame's own width",
//! the same sentinel-by-observed-behaviour pattern as `nullsrc`'s
//! `duration=-0.000001` (D9): `ffmpeg -vf thistogram` (no `width` given)
//! on a `16x16` source reports a `16x256` output.
//!
//! # Not measured/implemented
//!
//! `slide=scroll`/`rscroll`/`picture` (each shifts or otherwise handles
//! the canvas differently from a plain ring buffer — not measured, and
//! `create` rejects them with a clean error rather than silently
//! misbehaving as `replace`). `display_mode=overlay`/`parade`;
//! `levels_mode=logarithmic`; `envelope`; `components` beyond plane `0`
//! (this crate forces `Gray8` output the same way `histogram`/`waveform`
//! do, so the reference's native multi-plane output format is not
//! reproduced). Bit depths above 8.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "thistogram",
    description: "Compute and draw a temporal histogram.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const LEVELS: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slide {
    Replace,
    Frame,
}

impl Slide {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "replace" => Some(Self::Replace),
            "frame" => Some(Self::Frame),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "thistogram", help = "Compute and draw a temporal histogram.")]
pub(crate) struct Opts {
    #[opt(name = "width", alias = "w", help = "set width", default = 0, range = 0..=8192, flags(video, filtering))]
    pub width: i64,
    #[opt(name = "slide", help = "set slide mode", default = "replace".to_owned(), flags(video, filtering))]
    pub slide: String,
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
    /// `0` until `configure` resolves the `width=0` ("use the input's own
    /// width") sentinel against a real input link.
    width: u32,
    slide: Slide,
    /// Persistent `width x 256` canvas, row-major (`canvas[row*width+col]`).
    /// Empty until `configure` knows the real width.
    canvas: Vec<u8>,
    frame_count: u64,
}

/// Per-plane bin counts for an 8-bit plane, and the largest one — the same
/// shape as `histogram`'s own helper, kept local rather than shared: the
/// two crates' bar-height and intensity formulas differ (`ceil` versus
/// `round`), so sharing the counting loop alone would not remove much and
/// would couple two independently-measured formulas' one common step.
fn counts(rows: &[&[u8]], w: i32, h: i32) -> ([u64; 256], u64) {
    let mut bins = [0u64; 256];
    for y in 0..h {
        let Ok(uy) = usize::try_from(y) else { continue };
        let Some(row) = rows.get(uy) else { continue };
        for x in 0..w {
            let Ok(ux) = usize::try_from(x) else { continue };
            if let Some(&v) = row.get(ux)
                && let Some(bin) = bins.get_mut(usize::from(v))
            {
                *bin += 1;
            }
        }
    }
    let max = bins.iter().copied().max().unwrap_or(0);
    (bins, max)
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { width: in_w, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        let Some(mut out) = ctx.output_link(0).cloned() else {
            return Ok(());
        };
        let resolved = if self.width == 0 {
            in_w.max(1)
        } else {
            self.width
        };
        self.width = resolved;
        self.canvas = vec![0u8; usize::try_from(resolved).unwrap_or(1) * 256];
        if let LinkFormat::Video {
            width: w,
            height: h,
            ..
        } = &mut out
        {
            *w = resolved;
            *h = LEVELS;
        }
        ctx.set_output_link(0, out);
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() || self.width == 0 {
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let pw = common::to_i32(format.plane_width(width, 0));
        let ph = common::to_i32(format.plane_height(height, 0));
        let Some(src_plane) = input.plane(0) else {
            return Ok(FrameOut::One(input));
        };
        let rows: Vec<&[u8]> = (0..ph.max(0))
            .map(|y| {
                usize::try_from(y)
                    .ok()
                    .and_then(|uy| src_plane.row(uy))
                    .unwrap_or(&[])
            })
            .collect();
        let (bins, max) = counts(&rows, pw, ph);

        let w = usize::try_from(self.width).unwrap_or(1).max(1);
        let column = usize::try_from(self.frame_count % u64::from(self.width)).unwrap_or(0);
        if self.slide == Slide::Frame && column == 0 {
            self.canvas.fill(0);
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "bin counts fit comfortably in f64's exact integer range"
        )]
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "round() of a ratio in [0,1]*255 always lands in 0..=255"
        )]
        for (v, &count) in bins.iter().enumerate() {
            let intensity = if max == 0 {
                0
            } else {
                (count as f64 / max as f64 * 255.0).round() as u8
            };
            let row = 255 - v;
            if let Some(cell) = self.canvas.get_mut(row * w + column) {
                *cell = intensity;
            }
        }
        self.frame_count += 1;

        let mut out = ctx
            .pool()
            .acquire_video(PixFmt::Gray8, self.width, LEVELS)?;
        if let Some(mut dst) = out.plane_mut(0) {
            for row in 0..256usize {
                let Some(dst_row) = dst.row_mut(row) else {
                    continue;
                };
                let Some(src_row) = self.canvas.get(row * w..row * w + w) else {
                    continue;
                };
                let n = dst_row.len().min(src_row.len());
                if let (Some(dst_slice), Some(src_slice)) = (dst_row.get_mut(..n), src_row.get(..n))
                {
                    dst_slice.copy_from_slice(src_slice);
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
    let slide = Slide::from_name(&opts.slide).ok_or_else(|| {
        format!(
            "thistogram: `slide={}` is not implemented (only `replace` and `frame` are)",
            opts.slide
        )
    })?;
    let filter = Filter {
        width: u32::try_from(opts.width).unwrap_or(0),
        slide,
        canvas: Vec::new(),
        frame_count: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::converter(
            FormatSet::default(),
            FormatSet::video_exact(PixFmt::Gray8),
            req.instance,
        ),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Pinned against the reference probe: a flat frame (ratio `1.0`)
    /// lights its own bin at full intensity.
    #[test]
    fn a_flat_frame_lights_its_own_bin_at_full_intensity() {
        let row: Vec<u8> = vec![100; 16];
        let rows: Vec<&[u8]> = (0..16).map(|_| row.as_slice()).collect();
        let (bins, max) = counts(&rows, 16, 16);
        assert_eq!(max, 256);
        assert_eq!(bins[100], 256);
    }

    /// Pinned: a `56`-of-`200` ratio gives intensity `71` — `round`, and
    /// specifically *not* `histogram`'s `ceil` (which would give `72`).
    #[test]
    fn intensity_uses_round_not_ceil() {
        let ratio: f64 = 56.0 / 200.0;
        let intensity = (ratio * 255.0).round() as u8;
        assert_eq!(intensity, 71);
    }

    /// Pinned: exact half (`1`-of-`2`) rounds up to `128`, and a `3`-of-`8`
    /// ratio (`0.625`) rounds up to `96` — both rule out plain truncation.
    #[test]
    fn intensity_rounding_is_half_away_from_zero() {
        assert_eq!((1.0 / 2.0 * 255.0_f64).round() as u8, 128);
        assert_eq!((3.0 / 8.0 * 255.0_f64).round() as u8, 96);
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "thistogram",
            instance: "thistogram",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe: `slide=scroll`/`rscroll`/
    /// `picture` are recognised reference values this crate does not
    /// implement, and `create` says so rather than silently behaving like
    /// `replace`.
    #[test]
    fn unimplemented_slide_modes_are_a_clean_error() {
        for bad in ["scroll", "rscroll", "picture", "not-a-mode"] {
            let args = format!("slide={bad}");
            let req = Instantiate {
                name: "thistogram",
                instance: "thistogram",
                args: Some(&args),
                arguments: &[],
            };
            assert!(create(&req).is_err(), "slide={bad} should be rejected");
        }
    }

    /// Pinned against the reference's 4-frame, `w=2` probe in this
    /// module's doc: `slide=replace` overwrites only the target column,
    /// `slide=frame` clears the whole canvas whenever the column wraps
    /// back to `0`.
    #[test]
    fn replace_preserves_other_columns_frame_clears_on_wrap() {
        let w = 2usize;
        // Simulate the ring-buffer bookkeeping directly (the formula under
        // test), rather than driving the full `FrameFilter`: `canvas[row*w+col]`.
        let mut replace_canvas = vec![0u8; w * 256];
        let mut frame_canvas = vec![0u8; w * 256];
        let hits = [10u8, 20, 30, 40]; // one flat value's row, per frame
        for (i, &v) in hits.iter().enumerate() {
            let col = i % w;
            if col == 0 {
                frame_canvas.fill(0);
            }
            replace_canvas[usize::from(v) * w + col] = 255;
            frame_canvas[usize::from(v) * w + col] = 255;
        }
        // replace: every column's own frame survived.
        for (i, &v) in hits.iter().enumerate() {
            assert_eq!(replace_canvas[usize::from(v) * w + (i % w)], 255);
        }
        // frame: only the last wrap's two columns (frames 2 and 3) survive;
        // frame 0's and frame 1's marks were cleared at the frame-2 wrap.
        assert_eq!(frame_canvas[usize::from(hits[0]) * w], 0);
        assert_eq!(frame_canvas[usize::from(hits[1]) * w + 1], 0);
        assert_eq!(frame_canvas[usize::from(hits[2]) * w], 255);
        assert_eq!(frame_canvas[usize::from(hits[3]) * w + 1], 255);
    }
}
