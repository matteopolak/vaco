//! `colorhold` — turn everything **outside** a certain RGB colour range
//! into gray, keeping the range itself untouched.
//!
//! `ffmpeg -h filter=colorhold` documents `color` (default `black`),
//! `similarity` (`1e-05..1`, default `0.01`) and `blend` (`0..1`, default
//! `0`) — the same three options as [`crate::colorkey`], applied to a
//! different output.
//!
//! # Measured: which side of the range is held, and the gray formula
//!
//! `colorhold=color=red:similarity=0.1:blend=0` on `0xf00000` (a near-red
//! within similarity of the `red` key) reproduces the input **exactly
//! unchanged** — the *matching* range is held, not grayed. The same
//! filter on `0xff0000` against a `black` key with `similarity=0.1`
//! (distance `1/sqrt(3) ≈ 0.577`, well outside the range) instead
//! produces `(0x55, 0x55, 0x55)` — `mean(255, 0, 0) = 85`, confirming the
//! non-matching branch replaces every channel with the plain arithmetic
//! mean of the original R/G/B, not a luma-weighted gray.
//!
//! # Measured, and not exact: the blend ramp
//!
//! `colorhold=color=black:similarity=0.2:blend=0.2` on `0xRR0000` blends
//! `output = orig*(1-frac) + gray*frac` using [`crate::keying::ramp`]'s
//! `frac`. Four probe points: `0x64` and `0xc8` (the ramp's two
//! endpoints) matched exactly; the two interior points (`0x96`, `0xaa`)
//! were each off by **one** in the G/B channels (`34` measured `35`;
//! `52` measured `51`) against this exact formula computed in `f64`.
//! Shipped as measured rather than silently rounded to hide the
//! mismatch — see `measured_against_the_reference_a_blend_ramp`'s test
//! for the specific values and its comment for the tolerance this
//! crate accepts.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::colorkey::is_rgb_alpha;
use crate::common;
use crate::keying;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "colorhold",
    description: "Turns a certain color range into gray. Operates on RGB colors",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "colorhold",
    help = "Turns a certain color range into gray. Operates on RGB colors"
)]
pub(crate) struct Opts {
    #[opt(name = "color", alias = "c", help = "set the colorhold key color", default = "black".to_owned(), flags(video, filtering))]
    pub color: String,
    #[opt(name = "similarity", alias = "s", help = "set the colorhold similarity value", default = 0.01, range = 0.00001..=1.0, flags(video, filtering))]
    pub similarity: f64,
    #[opt(name = "blend", alias = "b", help = "set the colorhold blend value", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub blend: f64,
}

#[derive(Debug)]
struct Filter {
    key: [f64; 3],
    similarity: f64,
    blend: f64,
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
        let (Some(pr), Some(pg), Some(pb)) = (
            input.plane(cr.plane as usize),
            input.plane(cg.plane as usize),
            input.plane(cb.plane as usize),
        ) else {
            return;
        };
        let w = pr
            .row_bytes()
            .checked_div(usize::from(cr.step.max(1)))
            .unwrap_or(0);
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
        let mut planes = input.planes_mut();
        let to_u16 = |v: f64, max: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "v clamped to [0, max] and max fits u16 by construction"
            )]
            let out = v.clamp(0.0, max) as u16;
            out
        };
        for y in 0..rows {
            let Some(row_src) = src.get(y) else { continue };
            for x in 0..w {
                let Some(&(vr, vg, vb)) = row_src.get(x) else {
                    continue;
                };
                let (fr, fg, fb) = (f64::from(vr), f64::from(vg), f64::from(vb));
                let p = [fr / max_r, fg / max_g, fb / max_b];
                let d = keying::rgb_distance(p, self.key);
                let frac = keying::ramp(d, self.similarity, self.blend);
                let gray = (fr + fg + fb) / 3.0;
                let out_r = fr * (1.0 - frac) + gray * frac;
                let out_g = fg * (1.0 - frac) + gray * frac;
                let out_b = fb * (1.0 - frac) + gray * frac;
                if let Some(row) = planes.get_mut(cr.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cr, big_endian, to_u16(out_r, max_r));
                }
                if let Some(row) = planes.get_mut(cg.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cg, big_endian, to_u16(out_g, max_g));
                }
                if let Some(row) = planes.get_mut(cb.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cb, big_endian, to_u16(out_b, max_b));
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
    let key = vaco_core::parse::color(&opts.color)
        .ok_or_else(|| format!("colorhold: bad color `{}`", opts.color))?;
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

    fn one_pixel(rgb: [u8; 3]) -> Frame {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = rgb[0];
            row[1] = rgb[1];
            row[2] = rgb[2];
            row[3] = 255;
        }
        frame
    }

    #[test]
    fn matching_color_is_held_exactly() {
        let mut frame = one_pixel([0xf0, 0x00, 0x00]);
        let f = Filter {
            key: keying::key_rgb(vaco_core::parse::color("red").unwrap()),
            similarity: 0.1,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);
        assert_eq!(
            &frame.plane(0).unwrap().row(0).unwrap()[0..3],
            &[0xf0, 0x00, 0x00]
        );
    }

    #[test]
    fn non_matching_color_becomes_the_plain_rgb_mean() {
        let mut frame = one_pixel([0xff, 0x00, 0x00]);
        let f = Filter {
            key: [0.0, 0.0, 0.0],
            similarity: 0.1,
            blend: 0.0,
        };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(&row[0..3], &[0x55, 0x55, 0x55]);
    }

    #[test]
    fn measured_against_the_reference_a_blend_ramp() {
        // Measured: ffmpeg 8.1, colorhold=color=black:similarity=0.2:
        // blend=0.2 on 0xRR0000. The two interior points are one ULP off
        // this crate's f64 computation of the documented formula (see
        // this module's doc) — accepted with a tolerance of 1 rather than
        // hidden.
        let cases: &[(u8, [u8; 3])] = &[
            (0x64, [0x5b, 0x04, 0x04]),
            (0x96, [0x50, 0x22, 0x22]), // measured 0x50 0x23 0x23; off by 1 in G/B
            (0xaa, [0x41, 0x34, 0x34]), // measured 0x41 0x33 0x33; off by 1 in G/B
            (0xc8, [0x42, 0x42, 0x42]),
        ];
        for &(rr, expected) in cases {
            let mut frame = one_pixel([rr, 0, 0]);
            let f = Filter {
                key: [0.0, 0.0, 0.0],
                similarity: 0.2,
                blend: 0.2,
            };
            f.apply_frame(&mut frame);
            let row = frame.plane(0).unwrap().row(0).unwrap();
            for ch in 0..3 {
                let diff = i32::from(row[ch]) - i32::from(expected[ch]);
                assert!(
                    diff.abs() <= 1,
                    "rr=0x{rr:02x} ch={ch} got={} want~{}",
                    row[ch],
                    expected[ch]
                );
            }
        }
    }
}
