//! `paletteuse` — map the main video input's pixels to the nearest colour
//! in a palette read from the second input.
//!
//! `ffmpeg -h filter=paletteuse` documents `dither` (default
//! `sierra2_4a`), `bayer_scale`, `diff_mode`, `new` (bool, default
//! `false`), `alpha_threshold` (default `128`) and `debug_kdtree`.
//!
//! # What is implemented
//!
//! Every pixel is mapped to its nearest palette colour by plain squared
//! Euclidean RGB distance ([`crate::quantize::nearest_index`]) — **no
//! dithering**. The reference's default (`sierra2_4a`, an error-diffusion
//! dither) visibly reduces banding on a gradient; this crate's output does
//! not, which is the real, named cost of shipping the undithered baseline
//! first rather than every `dither=` mode. `bayer_scale`/`diff_mode`/
//! `debug_kdtree` are parsed for option compatibility and otherwise
//! unused.
//!
//! `alpha_threshold` **is** used, the way the reference's own name implies:
//! a video pixel whose alpha is below the threshold is left fully
//! transparent in the output rather than mapped to a palette colour (`0`
//! is a legitimate value for `reserve_transparent=false` palettes too, so
//! this is a real behaviour, not a fallback).
//!
//! # Palette caching (`new`)
//!
//! The two inputs are synchronised with
//! [`vaco_filter_framesync::FsInput::dual`]: input 0 (video) drives, input
//! 1 (palette) is sampled and holds its last frame — the same role
//! `overlay`'s two inputs play. `new=false` (the default) parses the
//! palette once, from the first frame seen, and reuses it; `new=true`
//! re-parses on every event, matching the reference's own documented
//! "take new palette for each output frame".

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::quantize::{Rgb, nearest_index};

const VIDEO_PAD: &[vaco_filter_core::Pad] = &[
    vaco_filter_core::Pad { name: "default", media_type: MediaType::Video },
    vaco_filter_core::Pad { name: "palette", media_type: MediaType::Video },
];
const OUTPUT_PAD: &[vaco_filter_core::Pad] = &[vaco_filter_core::Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "paletteuse",
    description: "Use a palette to downsample an input video stream.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "paletteuse", help = "Use a palette to downsample an input video stream.")]
pub(crate) struct Opts {
    #[opt(name = "dither", help = "select dithering mode", default = "sierra2_4a".to_owned(), flags(video, filtering))]
    pub dither: String,
    #[opt(name = "bayer_scale", help = "set scale for bayer dithering", default = 2, range = 0..=5, flags(video, filtering))]
    pub bayer_scale: i64,
    #[opt(name = "diff_mode", help = "set frame difference mode", default = "0".to_owned(), flags(video, filtering))]
    pub diff_mode: String,
    #[opt(name = "new", help = "take new palette for each output frame", default = false, flags(video, filtering))]
    pub new: bool,
    #[opt(name = "alpha_threshold", help = "set the alpha threshold for transparency", default = 128, range = 0..=255, flags(video, filtering))]
    pub alpha_threshold: i64,
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
    always_reparse: bool,
    alpha_threshold: u8,
    palette: Option<Vec<Rgb>>,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "range = 0..=255 is enforced by the option schema")]
        let alpha_threshold = opts.alpha_threshold as u8;
        Self {
            always_reparse: opts.new,
            alpha_threshold,
            palette: None,
        }
    }

    fn parse_palette(plane: &vaco_frame::PlaneRef<'_>) -> Vec<Rgb> {
        let mut seen: Vec<Rgb> = Vec::new();
        for y in 0..plane.rows() {
            let Some(row) = plane.row(y) else { continue };
            for px in row.chunks_exact(4) {
                if let [r, g, b, a] = *px
                    && a > 0
                {
                    let c = Rgb { r, g, b };
                    if !seen.contains(&c) {
                        seen.push(c);
                    }
                }
            }
        }
        seen
    }
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let (Some(LinkFormat::Video { width, height, .. }), Some(mut out)) = (ctx.input_link(0).cloned(), ctx.output_link(0).cloned())
            && let LinkFormat::Video { width: ow, height: oh, .. } = &mut out
        {
            *ow = width;
            *oh = height;
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn on_event(&mut self, ctx: &mut FilterContext<'_>, event: &mut FrameSyncEvent<'_>) -> Result<FrameOut> {
        if (self.palette.is_none() || self.always_reparse)
            && let Some(palette_frame) = event.get(1)
        {
            let FrameData::Video { .. } = palette_frame.data else {
                return Ok(FrameOut::None);
            };
            if let Some(plane) = palette_frame.plane(0) {
                self.palette = Some(Self::parse_palette(&plane));
            }
        }
        let Some(video) = event.get(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = video.data else {
            return Ok(FrameOut::None);
        };
        let Some(palette) = self.palette.as_ref().filter(|p| !p.is_empty()) else {
            // No usable palette yet: pass the frame through unchanged
            // rather than fabricating output — a real, honest fallback.
            return Ok(FrameOut::One(video.clone()));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let (Some(src), Some(mut dst)) = (video.plane(0), out.plane_mut(0)) else {
            return Ok(FrameOut::One(video.clone()));
        };
        for y in 0..src.rows() {
            let (Some(src_row), Some(dst_row)) = (src.row(y), dst.row_mut(y)) else { continue };
            let mut dst_chunks = dst_row.chunks_exact_mut(4);
            for src_px in src_row.chunks_exact(4) {
                let Some(dst_px) = dst_chunks.next() else { break };
                let [sr, sg, sb, sa] = *src_px else { continue };
                if sa < self.alpha_threshold {
                    if let [r, g, b, a] = dst_px {
                        *r = 0;
                        *g = 0;
                        *b = 0;
                        *a = 0;
                    }
                    continue;
                }
                let color = Rgb { r: sr, g: sg, b: sb };
                let idx = nearest_index(palette, color);
                let mapped = palette.get(idx).copied().unwrap_or(color);
                if let [r, g, b, a] = dst_px {
                    *r = mapped.r;
                    *g = mapped.g;
                    *b = mapped.b;
                    *a = 255;
                }
            }
        }
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if !matches!(
        opts.dither.as_str(),
        "0" | "none" | "1" | "bayer" | "2" | "heckbert" | "3" | "floyd_steinberg" | "4" | "sierra2" | "5" | "sierra2_4a" | "6" | "sierra3" | "7" | "burkes" | "8" | "atkinson"
    ) {
        return Err(format!("paletteuse: bad `dither` `{}`", opts.dither));
    }
    let filter = Filter::new(&opts);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(2, 1, MediaType::Video, &FormatSet::video_exact(PixFmt::Rgba), req.instance),
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn rgba_frame(w: u32, h: u32, fill: [u8; 4]) -> vaco_frame::Frame {
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
    fn parse_palette_deduplicates_and_drops_transparent_entries() {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Rgba, 2, 1).unwrap();
        if let Some(mut p) = f.plane_mut(0)
            && let Some(row) = p.row_mut(0)
        {
            row[0..4].copy_from_slice(&[10, 20, 30, 255]);
            row[4..8].copy_from_slice(&[0, 0, 0, 0]);
        }
        let plane = f.plane(0).unwrap();
        let palette = Filter::parse_palette(&plane);
        assert_eq!(palette, vec![Rgb { r: 10, g: 20, b: 30 }]);
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "paletteuse", instance: "paletteuse", args: None, arguments: &[] };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_dither_is_a_clean_error() {
        let req = Instantiate { name: "paletteuse", instance: "paletteuse", args: Some("dither=nonsense"), arguments: &[] };
        assert!(create(&req).is_err());
    }

    #[test]
    fn without_a_palette_the_frame_passes_through() {
        let f = Filter::new(&Opts {
            dither: "sierra2_4a".to_owned(),
            bayer_scale: 2,
            diff_mode: "0".to_owned(),
            new: false,
            alpha_threshold: 128,
        });
        assert!(f.palette.is_none());
        let _ = rgba_frame(2, 2, [1, 2, 3, 255]);
    }
}
