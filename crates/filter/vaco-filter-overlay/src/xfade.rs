//! `xfade` — cross-fade one video into another over a fixed duration.
//! `transition=fade` only; see this module's doc for what is not
//! attempted.
//!
//! `ffmpeg -h filter=xfade` (2026-08-28): `transition` (`-1..=57`, `58`
//! named transitions plus `custom`, default `fade`), `duration`
//! (`<duration>`, default `1`), `offset` (`<duration>`, default `0`),
//! `expr` (custom-transition expression). No `eof_action`/`shortest`
//! surface — its own `duration`/`offset` timing model instead, so this is
//! built like `blend`'s `Synced`/`FrameSyncFilter` shape but the actual
//! blend weight comes from elapsed time, not a fixed formula.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, two flat `black`/`white`
//! sources, `10fps`, hand-built `lavfi` inputs)
//!
//! ```text
//! progress = clamp((pts_seconds - offset) / duration, 0, 1)
//! fade(a, b, progress) = floor(a + progress*(b - a))
//! ```
//!
//! Pinned at all 10 frames of a 1-second, `10fps` transition
//! (`black -> white`): frame `i`'s value matches `floor(255 * i/10)`
//! exactly, including the non-tie fractional values (`25`, `76`, `127`,
//! `178`, `229` at `i = 1, 3, 5, 7, 9`) — the same `floor`, not `round`,
//! convention this crate's other per-pixel arithmetic uses. After the
//! transition window ends (`progress >= 1`), the reference holds the
//! second input's value exactly.
//!
//! # Not measured/implemented
//!
//! Every other named transition (`wipeleft`, `slideup`, `circlecrop`,
//! `distance`, `radial`, … — 57 in total) is its own per-pixel geometry
//! formula, not a variant of `fade`'s arithmetic, and none was measured
//! this pass. `expr` (custom transitions). `create` rejects any
//! `transition` other than `fade` with a clean error. Bit depths above 8.

use vaco_core::{Duration as VDuration, MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Video,
    },
    Pad {
        name: "xfade",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "xfade",
    description: "Cross fade one video with another video.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "xfade", help = "Cross fade one video with another video.")]
pub(crate) struct Opts {
    #[opt(name = "transition", help = "set cross fade transition", default = "fade".to_owned(), flags(video, filtering))]
    pub transition: String,
    #[opt(name = "duration", help = "set cross fade duration", default = VDuration::from_micros(1_000_000), flags(video, filtering))]
    pub duration: VDuration,
    #[opt(name = "offset", help = "set cross fade start relative to first input stream", default = VDuration::ZERO, flags(video, filtering))]
    pub offset: VDuration,
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
    duration_secs: f64,
    offset_secs: f64,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = main.data
        else {
            return Ok(FrameOut::One(main));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(main));
        }
        let Some(second) = event.get(1) else {
            return Ok(FrameOut::One(main));
        };
        let t = event
            .timestamp()
            .to_seconds(event.time_base())
            .unwrap_or(0.0);
        let progress = if self.duration_secs > 0.0 {
            ((t - self.offset_secs) / self.duration_secs).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let Some(a_plane) = main.plane(plane) else {
                continue;
            };
            let Some(b_plane) = second.plane(plane) else {
                continue;
            };
            let Some(mut dst) = out.plane_mut(plane) else {
                continue;
            };
            let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(a_row) = a_plane.row(uy) else {
                    continue;
                };
                let Some(b_row) = b_plane.row(uy) else {
                    continue;
                };
                let Some(dst_row) = dst.row_mut(uy) else {
                    continue;
                };
                let n = a_row.len().min(b_row.len()).min(dst_row.len());
                for x in 0..n {
                    let (Some(&a), Some(&b)) = (a_row.get(x), b_row.get(x)) else {
                        continue;
                    };
                    #[allow(clippy::cast_precision_loss, reason = "8-bit samples fit f64 exactly")]
                    let value = f64::from(a) + progress * (f64::from(b) - f64::from(a));
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "value is a convex combination of two bytes, clamped"
                    )]
                    let out_val = value.floor().clamp(0.0, 255.0) as u8;
                    if let Some(px) = dst_row.get_mut(x) {
                        *px = out_val;
                    }
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
    if opts.transition != "fade" {
        return Err(format!(
            "xfade: transition `{}` is not implemented (only `fade` is)",
            opts.transition
        ));
    }
    let filter = Filter {
        duration_secs: opts.duration.as_secs_f64(),
        offset_secs: opts.offset.as_secs_f64(),
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "xfade",
            instance: "xfade",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn unimplemented_transition_is_a_clean_error() {
        let req = Instantiate {
            name: "xfade",
            instance: "xfade",
            args: Some("transition=wipeleft"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference probe in this module's doc: a
    /// `10fps`, 1-second transition from `0` to `255` matches
    /// `floor(255 * i/10)` at every one of its 10 frames.
    #[test]
    fn fade_matches_the_ten_frame_reference_probe() {
        let expected = [0u8, 25, 51, 76, 102, 127, 153, 178, 204, 229];
        for (i, &want) in expected.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let progress = (i as f64) / 10.0;
            let value = 0.0 + progress * (255.0 - 0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let got = value.floor().clamp(0.0, 255.0) as u8;
            assert_eq!(got, want, "frame {i}");
        }
    }
}
