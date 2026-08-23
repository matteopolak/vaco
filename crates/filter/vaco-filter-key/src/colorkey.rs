//! `colorkey` — turn a certain RGB colour into transparency.
//!
//! `ffmpeg -h filter=colorkey` documents `color` (default `black`),
//! `similarity` (`1e-05..1`, default `0.01`) and `blend` (`0..1`, default
//! `0`). See [`crate::keying`] for the measured distance/ramp formula
//! this filter and [`crate::colorhold`] share.
//!
//! # Measured: output format
//!
//! ```text
//! ffmpeg -f lavfi -i "color=red:s=2x2,format=rgb24" -vf colorkey=color=black -f null -
//! # -> forces an alpha-capable RGB format (argb) regardless of input,
//! #    converting from YUV first if necessary.
//! ```
//!
//! # Not measured: interaction with pre-existing alpha
//!
//! Every probe used an opaque source, so whether `colorkey` multiplies
//! its computed alpha into an existing alpha channel or overwrites it
//! outright was not disambiguated. This implementation overwrites —
//! consistent with the reference always forcing a fresh alpha-capable
//! format, which suggests the alpha channel is being *produced* here
//! rather than composited with one that already existed — but this is a
//! documented assumption, not a confirmed match, in the same spirit as
//! `premultiply.rs`'s own flagged simplification.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::keying;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "colorkey",
    description: "Turns a certain color into transparency. Operates on RGB colors",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colorkey", help = "Turns a certain color into transparency. Operates on RGB colors")]
pub(crate) struct Opts {
    #[opt(name = "color", alias = "c", help = "set the colorkey key color", default = "black".to_owned(), flags(video, filtering))]
    pub color: String,
    #[opt(name = "similarity", alias = "s", help = "set the colorkey similarity value", default = 0.01, range = 0.00001..=1.0, flags(video, filtering))]
    pub similarity: f64,
    #[opt(name = "blend", alias = "b", help = "set the colorkey key blend value", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub blend: f64,
}

pub(crate) fn is_rgb_alpha(fmt: vaco_pixfmt::PixFmt) -> bool {
    fmt.is_rgb() && fmt.has_alpha() && sample::is_addressable(fmt)
}

#[derive(Debug)]
pub(crate) struct Filter {
    pub(crate) key: [f64; 3],
    pub(crate) similarity: f64,
    pub(crate) blend: f64,
}

impl Filter {
    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !is_rgb_alpha(format) {
            return;
        }
        let big_endian = format.is_big_endian();
        let n = format.component_count();
        let Some(alpha_comp) = sample::component(format, n.saturating_sub(1)) else {
            return;
        };
        let (Some(cr), Some(cg), Some(cb)) =
            (sample::component(format, 0), sample::component(format, 1), sample::component(format, 2))
        else {
            return;
        };
        let (max_r, max_g, max_b) = (
            f64::from(sample::max_value(cr)),
            f64::from(sample::max_value(cg)),
            f64::from(sample::max_value(cb)),
        );
        let (Some(pr), Some(pg), Some(pb)) =
            (input.plane(cr.plane as usize), input.plane(cg.plane as usize), input.plane(cb.plane as usize))
        else {
            return;
        };
        let w = pr.row_bytes().checked_div(usize::from(cr.step.max(1))).unwrap_or(0);
        let rows = pr.rows();
        let src: Vec<Vec<(u16, u16, u16)>> = (0..rows)
            .map(|y| {
                let (Some(rr), Some(rg), Some(rb)) = (pr.row(y), pg.row(y), pb.row(y)) else {
                    return Vec::new();
                };
                (0..w)
                    .map(|x| {
                        (
                            sample::read(rr, x, cr, big_endian),
                            sample::read(rg, x, cg, big_endian),
                            sample::read(rb, x, cb, big_endian),
                        )
                    })
                    .collect()
            })
            .collect();
        let alpha_max = f64::from(sample::max_value(alpha_comp));
        let mut planes = input.planes_mut();
        for y in 0..rows {
            let Some(row_src) = src.get(y) else { continue };
            for x in 0..w {
                let Some(&(vr, vg, vb)) = row_src.get(x) else { continue };
                let p = [f64::from(vr) / max_r, f64::from(vg) / max_g, f64::from(vb) / max_b];
                let d = keying::rgb_distance(p, self.key);
                let frac = keying::ramp(d, self.similarity, self.blend);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "frac in [0, 1], product in [0, alpha_max] and alpha_max fits u16 by construction; truncation is the measured reference rule"
                )]
                let alpha = (frac * alpha_max) as u16;
                if let Some(row) = planes.get_mut(alpha_comp.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, alpha_comp, big_endian, alpha);
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let key = vaco_core::parse::color(&opts.color).ok_or_else(|| format!("colorkey: bad color `{}`", opts.color))?;
    let set = FormatSet::video_list(common::formats_where(is_rgb_alpha));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter {
            key: keying::key_rgb(key),
            similarity: opts.similarity,
            blend: opts.blend,
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    #[test]
    fn exact_key_color_becomes_fully_transparent() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 0;
            row[1] = 0;
            row[2] = 0;
            row[3] = 255;
        }
        let f = Filter { key: [0.0, 0.0, 0.0], similarity: 0.5, blend: 0.0 };
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[3], 0);
    }

    #[test]
    fn far_color_stays_fully_opaque() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 255;
            row[1] = 255;
            row[2] = 255;
            row[3] = 255;
        }
        let f = Filter { key: [0.0, 0.0, 0.0], similarity: 0.01, blend: 0.0 };
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[3], 255);
    }

    #[test]
    fn measured_against_the_reference_a_blend_ramp() {
        // Measured: ffmpeg 8.1, colorkey=color=black:similarity=0.2:
        // blend=0.2 on 0xRR0000 (crate::keying's doc).
        let cases: &[(u8, u8)] = &[(0x64, 33), (0x85, 128), (0x96, 178), (0xaa, 235), (0xb1, 255)];
        for &(rr, expected_alpha) in cases {
            let mut budget = Budget::new(Limits::strict());
            let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
            {
                let mut p = frame.plane_mut(0).unwrap();
                let row = p.row_mut(0).unwrap();
                row[0] = rr;
                row[1] = 0;
                row[2] = 0;
                row[3] = 255;
            }
            let f = Filter { key: [0.0, 0.0, 0.0], similarity: 0.2, blend: 0.2 };
            f.apply_frame(&mut frame);
            assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[3], expected_alpha, "rr=0x{rr:02x}");
        }
    }

    #[test]
    fn rgb_channels_are_never_modified() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 12;
            row[1] = 34;
            row[2] = 56;
            row[3] = 255;
        }
        let f = Filter { key: [0.0, 0.0, 0.0], similarity: 0.9, blend: 0.0 };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(&row[0..3], &[12, 34, 56]);
    }
}
