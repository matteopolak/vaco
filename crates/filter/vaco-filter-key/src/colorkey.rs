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
//! # Measured, 2026-08-28: pre-existing alpha is overwritten, not composited
//!
//! The gap above was closed with a source that actually carries
//! non-trivial alpha before `colorkey` runs (every earlier probe used an
//! opaque source, which cannot distinguish the two hypotheses):
//!
//! ```text
//! ffmpeg -f lavfi -i "color=blue:s=4x4,format=argb,geq=a=200:r=0:g=0:b=255,format=argb" \
//!   -vf colorkey=color=black:similarity=0.01:blend=0 -f rawvideo -pix_fmt argb -
//! # -> pixel (unmatched colour, alpha 200 going in) comes back with
//! #    alpha 255, not 200: if the reference multiplied 200 into a
//! #    computed 255, an untouched pixel would keep its original 200.
//!
//! ffmpeg -f lavfi -i "color=black:s=4x4,format=argb,geq=a=200:r=0:g=0:b=0,format=argb" \
//!   -vf colorkey=color=black:similarity=0.01:blend=0 -f rawvideo -pix_fmt argb -
//! # -> pixel (matched colour, alpha 200 going in) comes back with alpha 0.
//! ```
//!
//! Both results are consistent only with **overwrite**, never with
//! multiplying into whatever alpha the pixel already carried (a
//! multiply against 255 for the unmatched case would still read back
//! `200`, not `255`). This confirms the implementation's existing
//! behaviour — `filter_frame` already calls [`sample::write`] on the
//! alpha component unconditionally, never reading it first — so no code
//! change was needed, only retiring the "documented assumption, not
//! confirmed" caveat this section used to carry.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::keying;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "colorkey",
    description: "Turns a certain color into transparency. Operates on RGB colors",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "colorkey",
    help = "Turns a certain color into transparency. Operates on RGB colors"
)]
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
        let (Some(cr), Some(cg), Some(cb)) = (
            sample::component(format, 0),
            sample::component(format, 1),
            sample::component(format, 2),
        ) else {
            return;
        };
        let (max_r, max_g, max_b) = (
            f64::from(sample::max_value(cr)),
            f64::from(sample::max_value(cg)),
            f64::from(sample::max_value(cb)),
        );
        let (w, rows) = {
            let Some(red) = input.plane(cr.plane as usize) else {
                return;
            };
            (
                red.row_bytes()
                    .checked_div(usize::from(cr.step.max(1)))
                    .unwrap_or(0),
                red.rows(),
            )
        };
        // RGB is never changed by colorkey, so only one row needs to outlive
        // the immutable source borrow before the alpha plane is written.
        let mut src = vec![(0u16, 0u16, 0u16); w];
        let alpha_max = f64::from(sample::max_value(alpha_comp));
        for y in 0..rows {
            {
                let (Some(pr), Some(pg), Some(pb)) = (
                    input.plane(cr.plane as usize),
                    input.plane(cg.plane as usize),
                    input.plane(cb.plane as usize),
                ) else {
                    return;
                };
                let (Some(rr), Some(rg), Some(rb)) = (pr.row(y), pg.row(y), pb.row(y)) else {
                    continue;
                };
                for (x, values) in src.iter_mut().enumerate() {
                    *values = (
                        sample::read(rr, x, cr, big_endian),
                        sample::read(rg, x, cg, big_endian),
                        sample::read(rb, x, cb, big_endian),
                    );
                }
            }
            let Some(mut alpha_plane) = input.plane_mut(alpha_comp.plane as usize) else {
                return;
            };
            let Some(alpha_row) = alpha_plane.row_mut(y) else {
                continue;
            };
            for x in 0..w {
                let Some(&(vr, vg, vb)) = src.get(x) else {
                    continue;
                };
                let p = [
                    f64::from(vr) / max_r,
                    f64::from(vg) / max_g,
                    f64::from(vb) / max_b,
                ];
                let d = keying::rgb_distance(p, self.key);
                let frac = keying::ramp(d, self.similarity, self.blend);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "frac in [0, 1], product in [0, alpha_max] and alpha_max fits u16 by construction; truncation is the measured reference rule"
                )]
                let alpha = (frac * alpha_max) as u16;
                sample::write(alpha_row, x, alpha_comp, big_endian, alpha);
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
    let key = vaco_core::parse::color(&opts.color)
        .ok_or_else(|| format!("colorkey: bad color `{}`", opts.color))?;
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
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.5,
            blend: 0.0,
        };
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
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.01,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[3], 255);
    }

    /// Pinned against the reference probe in this module's doc, 2026-08-28:
    /// pre-existing alpha is overwritten, not multiplied against the
    /// computed key alpha. A starting alpha of `200` on an unmatched pixel
    /// must come back `255` (overwrite), not `200` (an untouched multiply
    /// against a computed factor of `1.0` would leave it at `200`).
    #[test]
    fn pre_existing_alpha_is_overwritten_not_multiplied() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            // Blue, far from the black key -- fully unmatched.
            row[0] = 0;
            row[1] = 0;
            row[2] = 255;
            row[3] = 200;
        }
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.01,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[3], 255);
    }

    #[test]
    fn measured_against_the_reference_a_blend_ramp() {
        // Measured: ffmpeg 8.1, colorkey=color=black:similarity=0.2:
        // blend=0.2 on 0xRR0000 (crate::keying's doc).
        let cases: &[(u8, u8)] = &[
            (0x64, 33),
            (0x85, 128),
            (0x96, 178),
            (0xaa, 235),
            (0xb1, 255),
        ];
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
            let f = Filter {
                key: [0.0, 0.0, 0.0],
                similarity: 0.2,
                blend: 0.2,
            };
            f.apply_frame(&mut frame);
            assert_eq!(
                frame.plane(0).unwrap().row(0).unwrap()[3],
                expected_alpha,
                "rr=0x{rr:02x}"
            );
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
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.9,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(&row[0..3], &[12, 34, 56]);
    }

    #[test]
    fn multiple_rows_keep_each_pixels_rgb_while_replacing_alpha() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 3, 2).unwrap();
        let input = [
            [0, 0, 0, 12],
            [255, 0, 0, 34],
            [0, 255, 0, 56],
            [0, 0, 255, 78],
            [0, 0, 0, 90],
            [255, 255, 255, 123],
        ];
        for (y, pixels) in input.chunks_exact(3).enumerate() {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(y).unwrap();
            for (x, pixel) in pixels.iter().enumerate() {
                let start = x * 4;
                row[start..start + 4].copy_from_slice(pixel);
            }
        }
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.01,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);

        let expected_alpha = [0, 255, 255, 255, 0, 255];
        for (y, pixels) in input.chunks_exact(3).enumerate() {
            let row = frame.plane(0).unwrap().row(y).unwrap();
            for (x, pixel) in pixels.iter().enumerate() {
                let index = y * 3 + x;
                let start = x * 4;
                assert_eq!(&row[start..start + 3], &pixel[..3]);
                assert_eq!(row[start + 3], expected_alpha[index]);
            }
        }
    }
}
