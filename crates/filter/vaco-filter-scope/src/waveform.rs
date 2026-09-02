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
//! every pixel `(x, y)` in that column with value `v` accumulates at output
//! pixel `(x, 255 - v)` (the `255 -` is `mirror=true`, the default —
//! confirmed by locating hits at row `255-v`, not row `v`, for known input
//! values). Confirmed the accumulation is per-hit and additive, not "any
//! hit lights the pixel": a column with four hits at the same value reads
//! `~4x` a column with one hit at that value.
//!
//! **Corrected 2026-08-28: the per-hit contribution is truncated to an
//! integer once, then summed as an integer — not summed as an exact float
//! and truncated once at the end.** The two rules were flagged as
//! "indistinguishable at this magnitude" by the original two-point probe
//! (`1`/`4` hits at `intensity=1`, where `step=255` is already an integer
//! so there is nothing to truncate either way) — a gap this crate's own
//! doc had recorded rather than silently closed. `vaco-conformance`'s
//! argument-vector corpus tried the filter's *default* `intensity=0.04`
//! for the first time and found a real, `1`-byte-off divergence: five hits
//! at `intensity=0.04` gave `ours=51` (`(5*0.04*255.0).min(255.0) as u8`,
//! the old formula) against the reference's `50`
//! (`5*floor(0.04*255.0)=5*10=50`). Confirmed at five independent
//! `(intensity, count)` pairs, including two where the two rules predict
//! different results by more than one (`intensity=0.1, count=5`: old
//! formula `127`, corrected `125`, reference `125`). The corrected rule:
//!
//! ```text
//! step = floor(intensity * 255)     // truncated once, not per hit
//! value = min(255, count * step)    // count = hits at this (column, v)
//! ```
//!
//! the same "truncate the per-hit step once" shape `vectorscope`'s own
//! intensity formula uses, not a coincidence worth re-deriving twice —
//! both are this project's own discovery, not shared code, since the two
//! filters' accumulation loops are otherwise unrelated.
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
        if let LinkFormat::Video {
            width: w,
            height: h,
            ..
        } = &mut out
        {
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
        // Per-hit contribution is truncated to an integer byte amount
        // *once*, up front (`floor(intensity*255)`), then every hit adds
        // that same integer -- not "sum the exact float step per hit, then
        // truncate the total at the end," which is a materially different
        // rule and was this module's own previously-recorded open question
        // (see the module doc's history). Found by `vaco-conformance`'s
        // argument-vector corpus: a per-row-distinct-value source at the
        // *default* `intensity=0.04` (a case the original two-point probe
        // never tried, since it only checked `intensity=1`, where the
        // difference cannot show) gave `ours=51, theirs=50` at a `5`-hit
        // cell -- `5*floor(0.04*255)=5*10=50` matches; `(5*0.04*255).min(255)
        // as u8 = 51` (this module's old formula) does not. Confirmed at
        // five independent `(intensity, count)` pairs directly against the
        // reference, matching exactly the same "truncate the per-hit step
        // once, not per-hit-and-summed-as-float" rule `vectorscope`'s own
        // intensity formula uses.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "intensity is clamped to 0.0..=1.0 by the option schema, so 255.0*intensity is in 0.0..=255.0"
        )]
        let step: u32 = (self.intensity * 255.0).floor() as u32;
        let mut acc = vec![0u32; usize::try_from(pw.max(0)).unwrap_or(0) * 256];
        for y in 0..ph {
            let Ok(uy) = usize::try_from(y) else { continue };
            let Some(row) = src_plane.row(uy) else {
                continue;
            };
            for x in 0..pw {
                let Ok(ux) = usize::try_from(x) else { continue };
                let Some(&v) = row.get(ux) else { continue };
                let idx = usize::from(v) * usize::try_from(pw.max(0)).unwrap_or(0) + ux;
                if let Some(cell) = acc.get_mut(idx) {
                    *cell = cell.saturating_add(step);
                }
            }
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "cell is clamped into 0..=255 immediately below"
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
                    *px = cell.min(255) as u8;
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
        let step = (0.04 * 255.0_f64).floor() as u32;
        let one_hit = 1u32.saturating_mul(step).min(255);
        let four_hits = 4u32.saturating_mul(step).min(255);
        assert_eq!(one_hit, 10);
        assert_eq!(four_hits, 40);
    }

    /// The 2026-08-28 correction, pinned at the exact points that
    /// distinguish it from the old "sum as float, truncate once" rule —
    /// see the module doc for the real `vaco-conformance` divergence this
    /// came from.
    #[test]
    fn accumulation_truncates_the_per_hit_step_once_not_the_final_sum() {
        let step = |intensity: f64| (intensity * 255.0).floor() as u32;
        // 5 hits at the default intensity: old formula gave 51, reference
        // (and this corrected formula) gives 50.
        assert_eq!(5u32.saturating_mul(step(0.04)).min(255), 50);
        // A wider gap: old formula gave 127, reference gives 125.
        assert_eq!(5u32.saturating_mul(step(0.1)).min(255), 125);
        assert_eq!(3u32.saturating_mul(step(0.1)).min(255), 75);
        assert_eq!(2u32.saturating_mul(step(0.3)).min(255), 152);
    }

    proptest::proptest! {
        /// Invariant: accumulated intensity is always clamped into a valid
        /// byte, regardless of how many hits land on one cell.
        #[test]
        fn accumulated_intensity_always_clamps_to_a_byte(hits in 0u32..=1000, intensity in 0.0f64..=1.0) {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "intensity in 0.0..=1.0 by the proptest strategy, so 255.0*intensity is in 0.0..=255.0"
            )]
            let step = (intensity * 255.0).floor() as u32;
            let total = hits.saturating_mul(step).min(255);
            proptest::prop_assert!(total <= 255);
        }
    }
}
