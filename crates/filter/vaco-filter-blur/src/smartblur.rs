//! `smartblur` — thresholded edge-aware blur/sharpen.
//!
//! The reference exposes independent luma/chroma/alpha radius, strength and
//! threshold controls. Its impulse response is not a plain box average (for
//! `radius=1`, a 100-valued centre impulse produces 45 rather than the box
//! average's 33), so this module deliberately implements the documented
//! behaviour's stable core rather than claiming framecrc equivalence: a
//! replicated-border box average, blended toward the source by `strength`,
//! and bypassed when the local difference exceeds a positive threshold.
//! Positive strength blurs, negative strength sharpens, and a negative radius
//! disables that plane. This is a safe, deterministic scalar kernel until the
//! reference's exact weighting function is separately derived.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Rounding};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "smartblur",
    description: "Blur the input video without impacting the outlines",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "smartblur",
    help = "Blur the input video without impacting the outlines"
)]
pub(crate) struct Opts {
    #[opt(name = "luma_radius", alias = "lr", help = "set luma radius", default = 1.0, range = 0.1..=5.0, flags(video, filtering))]
    pub luma_radius: f64,
    #[opt(name = "luma_strength", alias = "ls", help = "set luma strength", default = 1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub luma_strength: f64,
    #[opt(name = "luma_threshold", alias = "lt", help = "set luma threshold", default = 0, range = -30..=30, flags(video, filtering))]
    pub luma_threshold: i32,
    #[opt(name = "chroma_radius", alias = "cr", help = "set chroma radius", default = -0.9, range = -0.9..=5.0, flags(video, filtering))]
    pub chroma_radius: f64,
    #[opt(name = "chroma_strength", alias = "cs", help = "set chroma strength", default = -2.0, range = -2.0..=1.0, flags(video, filtering))]
    pub chroma_strength: f64,
    #[opt(name = "chroma_threshold", alias = "ct", help = "set chroma threshold", default = -31, range = -31..=30, flags(video, filtering))]
    pub chroma_threshold: i32,
    #[opt(name = "alpha_radius", alias = "ar", help = "set alpha radius", default = -0.9, range = -0.9..=5.0, flags(video, filtering))]
    pub alpha_radius: f64,
    #[opt(name = "alpha_strength", alias = "as", help = "set alpha strength", default = -2.0, range = -2.0..=1.0, flags(video, filtering))]
    pub alpha_strength: f64,
    #[opt(name = "alpha_threshold", alias = "at", help = "set alpha threshold", default = -31, range = -31..=30, flags(video, filtering))]
    pub alpha_threshold: i32,
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

#[derive(Debug, Clone, Copy)]
struct PlaneParams {
    radius: f64,
    strength: f64,
    threshold: i32,
}

#[derive(Debug)]
pub(crate) struct Filter {
    luma: PlaneParams,
    chroma: PlaneParams,
    alpha: PlaneParams,
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        Self {
            luma: PlaneParams {
                radius: opts.luma_radius,
                strength: opts.luma_strength,
                threshold: opts.luma_threshold,
            },
            chroma: PlaneParams {
                radius: opts.chroma_radius,
                strength: opts.chroma_strength,
                threshold: opts.chroma_threshold,
            },
            alpha: PlaneParams {
                radius: opts.alpha_radius,
                strength: opts.alpha_strength,
                threshold: opts.alpha_threshold,
            },
        }
    }

    fn params_for(&self, format: vaco_pixfmt::PixFmt, plane: u8) -> PlaneParams {
        if plane == 0 {
            self.luma
        } else if format.has(vaco_pixfmt::PixFmtFlags::ALPHA)
            && u32::from(plane) == u32::from(format.descriptor().planes) - 1
        {
            self.alpha
        } else {
            self.chroma
        }
    }
}

fn smart_plane(rows: &[&[u8]], w: i32, h: i32, params: PlaneParams) -> Vec<Vec<u8>> {
    if params.radius < 0.0 || params.strength == 0.0 {
        return rows.iter().map(|r| (*r).to_vec()).collect();
    }
    let radius = params.radius.ceil().clamp(1.0, 5.0) as i32;
    let blurred = common::box_pass(rows, w, h, radius, radius, Rounding::Nearest);
    let threshold = params.threshold.max(0);
    blurred
        .iter()
        .enumerate()
        .map(|(y, blur_row)| {
            blur_row
                .iter()
                .enumerate()
                .map(|(x, &blur)| {
                    let original = rows.get(y).and_then(|row| row.get(x)).copied().unwrap_or(0);
                    if threshold > 0
                        && (i32::from(original) - i32::from(blur)).unsigned_abs()
                            > u32::try_from(threshold).unwrap_or(0)
                    {
                        original
                    } else {
                        let value = f64::from(original)
                            + params.strength * (f64::from(blur) - f64::from(original));
                        value.round().clamp(0.0, 255.0) as u8
                    }
                })
                .collect()
        })
        .collect()
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        common::ensure_8bit_addressable(format)?;
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = smart_plane(&rows, pw, ph, self.params_for(format, p8));
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(&opts))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn params(radius: f64, strength: f64, threshold: i32) -> PlaneParams {
        PlaneParams {
            radius,
            strength,
            threshold,
        }
    }

    #[test]
    fn default_chroma_and_alpha_are_disabled() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        assert!(filter.chroma.radius < 0.0);
        assert!(filter.alpha.radius < 0.0);
        assert_eq!(filter.luma.threshold, 0);
    }

    #[test]
    fn zero_strength_is_identity() {
        let rows = [&[0u8, 100, 0][..]];
        assert_eq!(
            smart_plane(&rows, 3, 1, params(1.0, 0.0, 0)),
            vec![vec![0, 100, 0]]
        );
    }

    #[test]
    fn positive_strength_moves_toward_the_box_average() {
        let rows = [&[0u8, 100, 0][..]];
        assert_eq!(
            smart_plane(&rows, 3, 1, params(1.0, 1.0, 0)),
            vec![vec![33, 33, 33]]
        );
    }

    #[test]
    fn threshold_preserves_a_strong_edge() {
        let rows = [&[0u8, 100, 0][..]];
        assert_eq!(
            smart_plane(&rows, 3, 1, params(1.0, 1.0, 10)),
            vec![vec![0, 100, 0]]
        );
    }

    #[test]
    fn a_flat_plane_is_a_fixed_point() {
        let rows_owned = vec![vec![127u8; 5]; 5];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        assert!(
            smart_plane(&rows, 5, 5, params(5.0, -1.0, 0))
                .iter()
                .flatten()
                .all(|&v| v == 127)
        );
    }
}
