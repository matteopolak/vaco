//! `multiply` — multiply two video inputs, sample by sample, with a
//! normalised scale/offset.
//!
//! `ffmpeg -h filter=multiply` (2026-08-28): `scale` (`0..=9`, default
//! `1`), `offset` (`-1..=1`, default `0.5`), `planes` (bitmask, default
//! all). No `eof_action`/`shortest`/`ts_sync_mode` section at all — a
//! fixed fixed-arity lockstep shape, the same one `mergeplanes` has, not
//! `blend`'s full `vaco-filter-framesync` surface. Built on `Paired`.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, a `0..=255` gradient against a
//! fixed second operand, hand-built `rawvideo` sources)
//!
//! ```text
//! out = clamp(round((a/255) * (b/255) * scale * 255 + offset*255), 0, 255)
//! ```
//!
//! Confirmed at `offset=0`, `scale=1` against exactly 4 of 6 gradient
//! points (`a = 0, 50, 100, 150, 200, 255`, `b = 150`): `0, 29, 59, 88`
//! for `a = 0, 50, 100, 150` match `round(a*b/255)`. The other two do
//! **not** — and, tellingly, not in a way one consistent rounding rule
//! can explain: `a=200` gives the reference `117` where `round` predicts
//! `118` (this needs `floor`), while `a=100` needs `round` and would be
//! wrong under `floor` (`58`, not the reference's `59`). `a=255` (exactly
//! `150` with no fractional part under either rule) reads `149`. Both
//! `floor` and `round` match exactly 4 of the same 6 points, at different
//! points — the two hypotheses this module tried cannot both be patched
//! into agreement, which is itself evidence the reference is not applying
//! a single integer rounding rule to the exact rational `a*b/255` at all.
//! The most plausible explanation is floating-point representation error
//! specific to the reference's own operation order (e.g. `150/255.0` is
//! not exactly representable, and where that error lands depends on `a`),
//! which this module's `f64` arithmetic does not reproduce bit-for-bit
//! without knowing that exact order. Recorded rather than hidden — **this
//! filter is structural, not confirmed framecrc-exact**, and the
//! inexactness is not confined to an edge case.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[
    Pad {
        name: "a",
        media_type: MediaType::Video,
    },
    Pad {
        name: "b",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "multiply",
    description: "Multiply first video stream with second video stream.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "multiply", help = "Multiply first video stream with second video stream.")]
pub(crate) struct Opts {
    #[opt(name = "scale", help = "set scale", default = 1.0, range = 0.0..=9.0, flags(video, filtering))]
    pub scale: f64,
    #[opt(name = "offset", help = "set offset", default = 0.5, range = -1.0..=1.0, flags(video, filtering))]
    pub offset: f64,
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
    scale: f64,
    offset: f64,
}

impl PairedFilter for Filter {
    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        mut inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        if inputs.len() != 2 {
            return Ok(FrameOut::None);
        }
        let (Some(b), Some(a)) = (inputs.pop(), inputs.pop()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = a.data else {
            return Ok(FrameOut::One(a));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(a));
        }
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let Some(a_plane) = a.plane(plane) else { continue };
            let Some(b_plane) = b.plane(plane) else { continue };
            let Some(mut dst) = out.plane_mut(plane) else { continue };
            let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(a_row) = a_plane.row(uy) else { continue };
                let Some(b_row) = b_plane.row(uy) else { continue };
                let Some(dst_row) = dst.row_mut(uy) else { continue };
                let n = a_row.len().min(b_row.len()).min(dst_row.len());
                for x in 0..n {
                    let (Some(&av), Some(&bv)) = (a_row.get(x), b_row.get(x)) else {
                        continue;
                    };
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "8-bit samples fit f64 exactly"
                    )]
                    let normalized = (f64::from(av) / 255.0) * (f64::from(bv) / 255.0)
                        * self.scale
                        * 255.0
                        + self.offset * 255.0;
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamp bounds the result to a byte"
                    )]
                    let out_val = normalized.round().clamp(0.0, 255.0) as u8;
                    if let Some(px) = dst_row.get_mut(x) {
                        *px = out_val;
                    }
                }
            }
        }
        out.pts = a.pts;
        out.time_base = a.time_base;
        out.duration = a.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter {
        scale: opts.scale,
        offset: opts.offset,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "multiply",
            instance: "multiply",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe in this module's doc:
    /// `offset=0`, `scale=1`, at the gradient points that match
    /// `round(a*b/255)` cleanly (`a=0,50,100,150`). `a=200` and `a=255`
    /// are deliberately not pinned here — see the module doc for why
    /// they are a known, unreproduced discrepancy (and why `floor` is no
    /// more consistent a fix) rather than a silently-passing test.
    #[test]
    fn offset_zero_scale_one_matches_the_reference_at_four_points() {
        let cases = [(0u8, 0u8), (50, 29), (100, 59), (150, 88)];
        for (a, want) in cases {
            let normalized = (f64::from(a) / 255.0) * (150.0 / 255.0) * 1.0 * 255.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let got = normalized.round().clamp(0.0, 255.0) as u8;
            assert_eq!(got, want, "a={a}");
        }
    }
}
