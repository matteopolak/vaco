//! `palettegen` — accumulate a colour histogram across every input frame
//! and emit a single `16x16` RGBA palette image at end of stream.
//!
//! `ffmpeg -h filter=palettegen` documents `max_colors` (`2..=256`, default
//! `256`), `reserve_transparent` (bool, default `true`),
//! `transparency_color` (default `lime`) and `stats_mode`
//! (`full`/`diff`/`single`, default `full`).
//!
//! # What is implemented
//!
//! `max_colors` and `reserve_transparent` both affect the output exactly as
//! named: the histogram (RGB only — alpha is not part of the quantised
//! colour) is reduced to `max_colors - 1` colours via
//! [`crate::quantize::median_cut`] when `reserve_transparent` is set (one
//! slot held back for a fully-transparent entry), or `max_colors`
//! otherwise. `transparency_color` and `stats_mode` are parsed for option
//! compatibility but not otherwise used: this pass always accumulates the
//! whole stream (`full`'s own behaviour) regardless of `stats_mode`, and
//! the reserved transparent slot is always plain `(0,0,0,0)` rather than
//! `transparency_color`'s RGB with alpha forced to `0` — a named
//! simplification, not a silent one.
//!
//! Output layout: up to 256 colours placed row-major into a `16x16` RGBA
//! image starting at `(0,0)`, matching the reference's own default `16x16`
//! grid (measured: `ffmpeg -f lavfi -i testsrc=size=32x32:duration=1:rate=1
//! -vf palettegen -f rawvideo -pix_fmt rgba - | wc -c` is exactly
//! `16*16*4` bytes). Unused cells (fewer than 256 real colours) are filled
//! with the same fully-transparent placeholder as the reserved slot.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::quantize::{Histogram, median_cut};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "palettegen",
    description: "Find the optimal palette for a given stream.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const SIDE: u32 = 16;
const SIDE_USIZE: usize = 16;
const CELLS: usize = SIDE_USIZE * SIDE_USIZE;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "palettegen", help = "Find the optimal palette for a given stream.")]
pub(crate) struct Opts {
    #[opt(name = "max_colors", help = "set the maximum number of colors to use in the palette", default = 256, range = 2..=256, flags(video, filtering))]
    pub max_colors: i64,
    #[opt(name = "reserve_transparent", help = "reserve a palette entry for transparency", default = true, flags(video, filtering))]
    pub reserve_transparent: bool,
    #[opt(name = "transparency_color", help = "set a background color for transparency", default = "lime".to_owned(), flags(video, filtering))]
    pub transparency_color: String,
    #[opt(name = "stats_mode", help = "set statistics mode", default = "full".to_owned(), flags(video, filtering))]
    pub stats_mode: String,
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
    hist: Histogram,
    max_colors: usize,
    reserve_transparent: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        if !matches!(opts.stats_mode.as_str(), "full" | "0" | "diff" | "1" | "single" | "2") {
            return Err(format!("palettegen: bad `stats_mode` `{}`", opts.stats_mode));
        }
        let max_colors = usize::try_from(opts.max_colors).unwrap_or(256).clamp(2, 256);
        Ok(Self {
            hist: Histogram::new(),
            max_colors,
            reserve_transparent: opts.reserve_transparent,
        })
    }

    fn build_output(&self, pool: &vaco_frame::FramePool) -> Option<Frame> {
        let k = if self.reserve_transparent {
            self.max_colors.saturating_sub(1)
        } else {
            self.max_colors
        };
        let palette = median_cut(&self.hist, k);
        let mut out = pool.acquire_video(PixFmt::Rgba, SIDE, SIDE).ok()?;
        let mut plane = out.plane_mut(0)?;
        for y in 0..plane.rows() {
            let Some(row) = plane.row_mut(y) else { continue };
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let idx = y.saturating_mul(SIDE_USIZE).saturating_add(x);
                if let [r, g, b, a] = px {
                    if let Some(color) = (idx < CELLS).then(|| palette.get(idx)).flatten() {
                        *r = color.r;
                        *g = color.g;
                        *b = color.b;
                        *a = 255;
                    } else {
                        *r = 0;
                        *g = 0;
                        *b = 0;
                        *a = 0;
                    }
                }
            }
        }
        Some(out)
    }

    /// The actual histogram accumulation, independent of [`FilterContext`]
    /// so it can be exercised directly in tests.
    fn accumulate(&mut self, frame: &Frame) {
        let FrameData::Video { .. } = frame.data else {
            return;
        };
        let Some(plane) = frame.plane(0) else {
            return;
        };
        for y in 0..plane.rows() {
            let Some(row) = plane.row(y) else { continue };
            for px in row.chunks_exact(4) {
                if let [r, g, b, _a] = *px {
                    self.hist.add(r, g, b);
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let vaco_filter_core::LinkFormat::Video { width, height, .. } = &mut out {
                *width = SIDE;
                *height = SIDE;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        self.accumulate(&frame);
        Ok(FrameOut::None)
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        if self.hist.is_empty() {
            return Ok(FrameOut::None);
        }
        // Only ever emit once: `build_output` is cheap enough not to need a
        // separate "already flushed" flag, but the histogram is cleared so
        // a second `flush` call (the adapter's own retry-until-empty
        // contract) returns `None` rather than the same frame twice.
        let out = self.build_output(ctx.pool());
        self.hist = Histogram::new();
        Ok(out.map_or(FrameOut::None, FrameOut::One))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &FormatSet::video_exact(PixFmt::Rgba), req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn rgba_frame(w: u32, h: u32, fill: [u8; 4]) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Rgba, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    for px in row.chunks_exact_mut(4) {
                        px.copy_from_slice(&fill);
                    }
                }
            }
        }
        f
    }

    #[test]
    fn a_flat_frame_produces_a_one_colour_palette() {
        let mut f = Filter::new(&Opts {
            max_colors: 256,
            reserve_transparent: false,
            transparency_color: "lime".to_owned(),
            stats_mode: "full".to_owned(),
        })
        .unwrap();
        f.accumulate(&rgba_frame(4, 4, [10, 20, 30, 255]));
        let pool = vaco_frame::FramePool::default();
        let out = f.build_output(&pool).unwrap();
        let plane = out.plane(0).unwrap();
        let row = plane.row(0).unwrap();
        assert_eq!(&row[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "palettegen", instance: "palettegen", args: None, arguments: &[] };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_stats_mode_is_a_clean_error() {
        let req = Instantiate { name: "palettegen", instance: "palettegen", args: Some("stats_mode=bogus"), arguments: &[] };
        assert!(create(&req).is_err());
    }
}
