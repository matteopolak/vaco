//! `elbg` — posterize a single frame to `codebook_length` colours.
//!
//! `ffmpeg -h filter=elbg` documents `codebook_length`/`l` (default `256`),
//! `nb_steps`/`n` (default `1`), `seed`/`s` (default `-1`), `pal8` (bool,
//! default `false`) and `use_alpha` (bool, default `false`).
//!
//! # This is median-cut, not the reference's actual ELBG
//!
//! The reference's Enhanced Linde–Buzo–Gray algorithm iteratively refines
//! a codebook: an initial split, `nb_steps` rounds of generalized-Lloyd
//! relaxation (reassign points to the nearest centroid, recompute
//! centroids), and a utility-based step that reassigns low-value cells to
//! high-error regions. This module implements
//! [`crate::quantize::median_cut`] instead — a different, simpler, one-shot
//! member of the same vector-quantisation family, built from general
//! algorithmic knowledge (Heckbert 1982) rather than the reference's
//! source (D6/D7). `nb_steps` and `seed` are parsed for option
//! compatibility but have no effect: median-cut is deterministic and does
//! not iterate. `pal8`/`use_alpha` are parsed but not implemented — output
//! stays in the input's own (forced `Rgba`) format, alpha untouched.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::quantize::{Histogram, median_cut, nearest_index};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "elbg",
    description: "Apply posterize effect, using the ELBG algorithm.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "elbg", help = "Apply posterize effect, using the ELBG algorithm.")]
pub(crate) struct Opts {
    #[opt(name = "codebook_length", alias = "l", help = "set codebook length", default = 256, range = 1..=8192, flags(video, filtering))]
    pub codebook_length: i64,
    #[opt(name = "nb_steps", alias = "n", help = "set max number of steps used to compute the mapping", default = 1, range = 1..=1_000_000, flags(video, filtering))]
    pub nb_steps: i64,
    #[opt(name = "seed", alias = "s", help = "set the random seed", default = -1, flags(video, filtering))]
    pub seed: i64,
    #[opt(name = "pal8", help = "set the pal8 output", default = false, flags(video, filtering))]
    pub pal8: bool,
    #[opt(name = "use_alpha", help = "use alpha channel for mapping", default = false, flags(video, filtering))]
    pub use_alpha: bool,
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
    /// The reference's range is `1..=INT_MAX`; capped here at a value large
    /// enough to be a no-op for any realistic use (above `16_777_216`,
    /// median-cut cannot produce more colours than 8-bit RGB has anyway)
    /// while keeping the per-frame histogram-and-cut cost bounded.
    codebook_length: usize,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> Self {
        let codebook_length = usize::try_from(opts.codebook_length).unwrap_or(256).clamp(1, 8192);
        Self { codebook_length }
    }

    fn posterize(&self, frame: &Frame, pool: &vaco_frame::FramePool) -> Option<Frame> {
        let FrameData::Video { format, width, height, .. } = frame.data else {
            return None;
        };
        let src = frame.plane(0)?;
        let mut hist = Histogram::new();
        for y in 0..src.rows() {
            let Some(row) = src.row(y) else { continue };
            for px in row.chunks_exact(4) {
                if let [r, g, b, _a] = *px {
                    hist.add(r, g, b);
                }
            }
        }
        if hist.is_empty() {
            return None;
        }
        let palette = median_cut(&hist, self.codebook_length);
        let mut out = pool.acquire_video(format, width, height).ok()?;
        {
            let mut dst = out.plane_mut(0)?;
            for y in 0..src.rows() {
                let (Some(src_row), Some(dst_row)) = (src.row(y), dst.row_mut(y)) else { continue };
                let mut dst_chunks = dst_row.chunks_exact_mut(4);
                for src_px in src_row.chunks_exact(4) {
                    let Some(dst_px) = dst_chunks.next() else { break };
                    let [r, g, b, a] = *src_px else { continue };
                    let idx = nearest_index(&palette, crate::quantize::Rgb { r, g, b });
                    let mapped = palette.get(idx).copied().unwrap_or(crate::quantize::Rgb { r, g, b });
                    if let [dr, dg, db, da] = dst_px {
                        *dr = mapped.r;
                        *dg = mapped.g;
                        *db = mapped.b;
                        *da = a;
                    }
                }
            }
        }
        Some(out)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        match self.posterize(&frame, ctx.pool()) {
            Some(mut out) => {
                out.pts = frame.pts;
                out.time_base = frame.time_base;
                out.duration = frame.duration;
                Ok(FrameOut::One(out))
            }
            None => Ok(FrameOut::One(frame)),
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts);
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

    fn two_color_frame(w: u32, h: u32) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Rgba, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        let fill = if x % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
                        px.copy_from_slice(&fill);
                    }
                }
            }
        }
        f
    }

    #[test]
    fn posterizing_to_one_colour_collapses_everything() {
        let f = Filter::new(&Opts { codebook_length: 1, nb_steps: 1, seed: -1, pal8: false, use_alpha: false });
        let pool = vaco_frame::FramePool::default();
        let out = f.posterize(&two_color_frame(4, 2), &pool).unwrap();
        let plane = out.plane(0).unwrap();
        let first = plane.row(0).unwrap()[0..3].to_vec();
        for y in 0..2 {
            let row = plane.row(y).unwrap();
            for px in row.chunks_exact(4) {
                assert_eq!(&px[0..3], first.as_slice());
            }
        }
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "elbg", instance: "elbg", args: None, arguments: &[] };
        assert!(create(&req).is_ok());
    }
}
