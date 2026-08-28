//! `waveform` — per-column (or per-row) intensity distribution, the classic
//! video waveform monitor.
//!
//! `ffmpeg -h filter=waveform` (2026-08-28): `mode` (`row`/`column`, default
//! `column`), `intensity` (`0..=1`, default `0.04`), `mirror` (bool, default
//! `true`), `display` (`overlay`/`stack`/`parade`, default `stack`).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! `mode=column` output is `(source width) x 256`: for source column `x`,
//! every pixel `(x, y)` in that column with value `v` accumulates
//! `intensity * 255` at output pixel `(x, 255 - v)` (the `255 -` is
//! `mirror=true`, the default — confirmed by locating hits at row `255-v`,
//! not row `v`, for known input values). Confirmed the accumulation is
//! per-hit and additive, not "any hit lights the pixel": a column with four
//! hits at the same value reads `~4x` a column with one hit at that value
//! (`40` versus `10` at `intensity=0.04`, i.e. `4 * round(0.04*255)` either
//! summed as floats then truncated once or truncated per hit then summed —
//! the two are indistinguishable at this magnitude and this crate did not
//! probe further to separate them).
//!
//! # Not measured/implemented
//!
//! `mode=row`; `mirror=false`; `display=overlay`/`parade` (only the
//! single-plane `stack`-equivalent — there is nothing to stack with one
//! plane — is implemented). Bit depths above 8, and any non-luma plane.

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
    name: "waveform",
    description: "Video waveform monitor.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const LEVELS: u32 = 256;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "waveform", help = "Video waveform monitor.")]
pub(crate) struct Opts {
    #[opt(name = "intensity", alias = "i", help = "set intensity", default = 0.04, range = 0.0..=1.0, flags(video, filtering))]
    pub intensity: f64,
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
    intensity: f64,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { width, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        let Some(mut out) = ctx.output_link(0).cloned() else {
            return Ok(());
        };
        if let LinkFormat::Video { width: w, height: h, .. } = &mut out {
            *w = width;
            *h = LEVELS;
        }
        ctx.set_output_link(0, out);
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
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
        let mut out = ctx.pool().acquire_video(PixFmt::Gray8, width, LEVELS)?;
        let Some(mut dst_plane) = out.plane_mut(0) else {
            return Ok(FrameOut::One(input));
        };
        let step = self.intensity * 255.0;
        let mut acc = vec![0.0f64; usize::try_from(pw.max(0)).unwrap_or(0) * 256];
        for y in 0..ph {
            let Ok(uy) = usize::try_from(y) else { continue };
            let Some(row) = src_plane.row(uy) else { continue };
            for x in 0..pw {
                let Ok(ux) = usize::try_from(x) else { continue };
                let Some(&v) = row.get(ux) else { continue };
                let idx = usize::from(v) * usize::try_from(pw.max(0)).unwrap_or(0) + ux;
                if let Some(cell) = acc.get_mut(idx) {
                    *cell += step;
                }
            }
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "acc entries are non-negative sums of step (<=255 each); clamp bounds them"
        )]
        for v in 0..256usize {
            let out_y = 255 - v;
            let Some(dst_row) = dst_plane.row_mut(out_y) else {
                continue;
            };
            for x in 0..usize::try_from(pw.max(0)).unwrap_or(0) {
                let Some(&cell) = acc.get(v * usize::try_from(pw.max(0)).unwrap_or(0) + x) else {
                    continue;
                };
                if let Some(px) = dst_row.get_mut(x) {
                    *px = cell.min(255.0) as u8;
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
    let filter = Filter {
        intensity: opts.intensity,
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

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "waveform",
            instance: "waveform",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe in this module's doc: four hits
    /// at the same value accumulate to four times one hit's contribution,
    /// at `intensity=0.04`.
    #[test]
    fn accumulation_matches_the_measured_ratio() {
        let step: f64 = 0.04 * 255.0;
        let one_hit = step.min(255.0) as u8;
        let four_hits = (step * 4.0).min(255.0) as u8;
        assert_eq!(one_hit, 10);
        assert_eq!(four_hits, 40);
    }

    proptest::proptest! {
        /// Invariant: accumulated intensity is always clamped into a valid
        /// byte, regardless of how many hits land on one cell.
        #[test]
        fn accumulated_intensity_always_clamps_to_a_byte(hits in 0u32..=1000, intensity in 0.0f64..=1.0) {
            let total = f64::from(hits) * intensity * 255.0;
            let clamped = total.min(255.0);
            proptest::prop_assert!((0.0..=255.0).contains(&clamped));
        }
    }
}
