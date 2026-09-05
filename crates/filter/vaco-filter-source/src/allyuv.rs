//! `allyuv` — every combination of an 8-bit Y, U and V value, in a
//! 4096×4096 `yuv444p` frame (4096×4096 = 256^3, same closed-form
//! requirement as [`crate::allrgb`]).
//!
//! No `size` option, for the same reason `allrgb` has none.
//!
//! # The formula (measured, not read)
//!
//! Recovered the same way as `allrgb`'s — see that module's doc for the
//! method. `x` and `y` each range over 0..4095 (12 bits, `v = x >> 3` is 9
//! bits, `fold = (x >> 11) & 1`):
//!
//! ```text
//! Y = fold == 0 ? (v & 0xFF) : (255 - (v & 0xFF))          // tent in x
//! V = y >> 4                                                // ramp in y
//! U = (fold << 7) | (hi3 << 4) | (y & 0xF)
//!     where hi3 = fold == 0 ? (x & 7) : (7 - (x & 7))
//! ```
//!
//! `Y` folds `x`'s 9-bit macro-position back into 8 bits (a plain
//! `x >> 3` would double-cover 0..255, so the top half mirrors); the
//! direction bit that fold discards is not lost, it reappears as `U`'s top
//! bit, with `x`'s low 3 bits mirrored alongside it. `V` needs no fold: `y`'s
//! top 8 bits already land on 0..255 once each (with multiplicity 16), and
//! that multiplicity is exactly `y`'s low 4 bits, which is `U`'s low nibble.
//! Every `(x, y)` pair produces a distinct `(Y, U, V)` triple and every
//! triple is reached (bijection).
//!
//! The formula was derived from one set of measurements (`x` and `y` swept
//! independently near 0, near the fold boundary at `x = 2048`, and near the
//! ends of the range) and then confirmed on two held-out points not used in
//! the derivation: `(100, 100) -> (Y=12, U=68, V=6)` and
//! `(2048, 2048) -> (Y=255, U=240, V=128)`. Both matched before either was
//! folded into the formula, which is the point of holding them out.
//!
//! **Exact.**

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);
const SIZE: u32 = 4096;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "allyuv", help = "generate all yuv colors")]
pub(crate) struct Opts {
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "allyuv",
    description: "Generate all yuv colors",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// `(Y, U, V)` for one pixel, per the formula in the module doc.
fn yuv_at(x: u32, y: u32) -> (u8, u8, u8) {
    let fold = (x >> 11) & 1;
    let v = (x >> 3) & 0xFF;
    #[allow(clippy::cast_possible_truncation, reason = "v is <= 0xFF")]
    let yy = if fold == 0 { v } else { 255 - v } as u8;
    #[allow(clippy::cast_possible_truncation, reason = "y >> 4 is <= 0xFF")]
    let vv = (y >> 4) as u8;
    let low3 = x & 0x7;
    let hi3 = if fold == 0 { low3 } else { 7 - low3 };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "each term is masked to fit its nibble"
    )]
    let uu = ((fold << 7) | (hi3 << 4) | (y & 0xF)) as u8;
    (yy, uu, vv)
}

#[derive(Debug)]
struct Source {
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = SIZE;
                *height = SIZE;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx.pool().acquire_video(PixFmt::Yuv444p, SIZE, SIZE)?;
        for plane_idx in 0..3usize {
            if let Some(mut plane) = frame.plane_mut(plane_idx) {
                for row_idx in 0..plane.rows() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "plane.rows() == SIZE, which fits in u32"
                    )]
                    let yy = row_idx as u32;
                    if let Some(row) = plane.row_mut(row_idx) {
                        for (x, px) in row.iter_mut().enumerate() {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "x < SIZE, which fits in u32"
                            )]
                            let xx = x as u32;
                            let (y_v, u_v, v_v) = yuv_at(xx, yy);
                            *px = match plane_idx {
                                0 => y_v,
                                1 => u_v,
                                _ => v_v,
                            };
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let rate = opts.rate.0;
    let total_frames = if opts.duration < VDuration::ZERO {
        None
    } else {
        Some(crate::frame_budget(opts.duration, rate))
    };
    let source = Source {
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Yuv444p)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn matches_measured_reference_points() {
        assert_eq!(yuv_at(0, 0), (0, 0, 0));
        assert_eq!(yuv_at(1, 0), (0, 16, 0));
        assert_eq!(yuv_at(0, 1), (0, 1, 0));
        assert_eq!(yuv_at(4095, 0), (0, 128, 0));
        assert_eq!(yuv_at(0, 4095), (0, 15, 255));
        assert_eq!(yuv_at(4095, 4095), (0, 143, 255));
        assert_eq!(yuv_at(256, 0), (32, 0, 0));
        assert_eq!(yuv_at(255, 0), (31, 112, 0));
        assert_eq!(yuv_at(64, 0), (8, 0, 0));
    }

    /// Held out of the derivation on purpose: confirming a formula on the
    /// same points used to build it is not an independent check (see the
    /// `tblend`/256-vs-255 caution in `planning/AGENT-CONSTRAINTS.md`).
    #[test]
    fn matches_held_out_points_not_used_to_derive_the_formula() {
        assert_eq!(yuv_at(100, 100), (12, 68, 6));
        assert_eq!(yuv_at(2048, 2048), (255, 240, 128));
    }

    /// The independent oracle: the true generator's defining property is
    /// that it is a bijection onto every `(Y, U, V)` triple, not merely that
    /// a handful of sampled points look right.
    #[test]
    fn every_yuv_triple_appears_exactly_once() {
        let mut seen: HashSet<(u8, u8, u8)> = HashSet::with_capacity((SIZE * SIZE) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                assert!(seen.insert(yuv_at(x, y)));
            }
        }
        assert_eq!(seen.len(), 1 << 24);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "allyuv",
            instance: "allyuv",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
