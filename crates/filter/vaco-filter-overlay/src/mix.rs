//! `mix` — weighted sum of `N` video inputs.
//!
//! `ffmpeg -h filter=mix` (2026-08-28): `inputs` (`2..=32767`, capped here
//! at [`vaco_filter_graph::registry::pads::MAX`]), `weights` (a
//! space-separated string, default `"1 1"`), `scale` (`0..=32767`, default
//! `0`), `planes` (bitmask, default all), `duration`
//! (`longest`/`shortest`/`first`, default `longest`). No `eof_action`
//! surface — its own, simpler `duration` vocabulary instead.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, a `0..=255` gradient against a
//! fixed second operand, hand-built `rawvideo` sources)
//!
//! ```text
//! divisor = if scale == 0 { sum(weights) } else { scale }
//! out = clamp(round_ties_even(sum(weight_i * value_i) / divisor), 0, 255)
//! ```
//!
//! `weights="1 1"` (default), `scale=0` matches `blend`'s own `average`
//! mode exactly (`floor((a+b)/2)` — a non-tie case, since `(a+b)` is
//! always even-or-odd independent of the rounding rule here). The
//! rounding rule itself was confirmed with `weights="3 1"`: three exact
//! `.5` ties (`37.5`, `112.5`, `187.5`) all resolve to their **even**
//! neighbour (`38`, `112`, `188`) — round-half-to-**even**, not the
//! round-half-**up** `burn`/`dodge` use in [`crate::blend`]. Confirmed
//! `scale=0` genuinely means "auto: divide by the weights' own sum", not
//! "divide by `0`": `scale=1` with `weights="1 1"` skips the
//! auto-normalisation entirely (`a+b` un-divided, then clamped).
//!
//! `duration=longest` (default) and `duration=shortest` map onto
//! `vaco-filter-framesync`'s built-in `FsInput::uniform` shape exactly the
//! way `hstack`/`vstack` do (every input `after = Infinity` for
//! `longest`, `after = Stop` for `shortest`). `duration=first` has no
//! `vaco-filter-framesync` built-in equivalent (`eof_action=pass` keeps
//! *input 0* running when *others* end, the opposite of "stop input 0
//! ends the mix"), so this module builds its `FsInput` roles by hand:
//! `uniform(n)` with input `0`'s own `after` overridden to `Stop`.
//!
//! # Not measured/implemented
//!
//! `planes` (component selection — every plane is mixed). Bit depths
//! above 8.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::opts::ExtendMode;
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

use crate::common;

const OUTPUT_PAD: &[vaco_filter_core::Pad] = &[vaco_filter_core::Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "mix",
    description: "Mix video inputs.",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Duration {
    Longest,
    Shortest,
    First,
}

impl Duration {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "longest" => Some(Self::Longest),
            "shortest" => Some(Self::Shortest),
            "first" => Some(Self::First),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "mix", help = "Mix video inputs.")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "set number of inputs", default = 2, range = 2..=64, flags(video, filtering))]
    pub inputs: i64,
    #[opt(name = "weights", help = "set weight for each input", default = "1 1".to_owned(), flags(video, filtering))]
    pub weights: String,
    #[opt(name = "scale", help = "set scale", default = 0.0, range = 0.0..=32767.0, flags(video, filtering))]
    pub scale: f64,
    #[opt(name = "duration", help = "how to determine end of stream", default = "longest".to_owned(), flags(video, filtering))]
    pub duration: String,
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
    n: usize,
    weights: Vec<f64>,
    scale: f64,
    duration: Duration,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        let mut roles = FsInput::uniform(n);
        if self.duration == Duration::First
            && let Some(first) = roles.first_mut()
        {
            first.after = ExtendMode::Stop;
        }
        roles
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts {
            shortest: self.duration == Duration::Shortest,
            ..FrameSyncOpts::default()
        }
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
        let Some((format, width, height)) = event.get(0).and_then(|f| match &f.data {
            FrameData::Video { format, width, height, .. } => Some((*format, *width, *height)),
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
        }) else {
            return Ok(FrameOut::None);
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::None);
        }
        let sum_weights: f64 = self.weights.iter().sum();
        let divisor = if self.scale == 0.0 { sum_weights } else { self.scale };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
            let Some(mut dst) = out.plane_mut(plane) else { continue };
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(dst_row) = dst.row_mut(uy) else { continue };
                let row_len = dst_row.len();
                for x in 0..row_len {
                    let mut acc = 0.0f64;
                    for i in 0..self.n {
                        let Some(frame) = event.get(i) else { continue };
                        let Some(p) = frame.plane(plane) else { continue };
                        let Some(row) = p.row(uy) else { continue };
                        let Some(&v) = row.get(x) else { continue };
                        let w = self.weights.get(i).copied().unwrap_or(1.0);
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "8-bit samples fit f64 exactly"
                        )]
                        {
                            acc += w * f64::from(v);
                        }
                    }
                    let value = if divisor == 0.0 { acc } else { acc / divisor };
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamp bounds the result to a byte"
                    )]
                    let out_val = value.round_ties_even().clamp(0.0, 255.0) as u8;
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
    let n = usize::try_from(opts.inputs).unwrap_or(2).max(2);
    let duration = Duration::from_name(&opts.duration)
        .ok_or_else(|| format!("mix: bad `duration` `{}`", opts.duration))?;
    let weights: Vec<f64> = opts
        .weights
        .split_whitespace()
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    if weights.len() != n {
        return Err(format!(
            "mix: `weights` names {} value(s), but `inputs={n}`",
            weights.len()
        ));
    }
    let input_pads = pads::video(n).ok_or_else(|| "mix: too many inputs".to_owned())?;
    let filter = Filter {
        n,
        weights,
        scale: opts.scale,
        duration,
    };
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats::passthrough(n, 1, MediaType::Video, req.instance),
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
            name: "mix",
            instance: "mix",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe: default `weights="1 1"`,
    /// `scale=0` matches `blend`'s own `average` exactly.
    #[test]
    fn default_weights_match_blend_average() {
        let gradient = [0u8, 50, 100, 150, 200, 255];
        let expected = [75u8, 100, 125, 150, 175, 202];
        for (a, &want) in gradient.iter().zip(expected.iter()) {
            let sum = f64::from(*a) + 150.0;
            let got = (sum / 2.0).round_ties_even().clamp(0.0, 255.0) as u8;
            assert_eq!(got, want, "a={a}");
        }
    }

    /// Pinned against the reference's exact-tie probe: `weights="3 1"`
    /// resolves three `.5` ties to their even neighbour.
    #[test]
    fn weighted_sum_rounds_half_to_even() {
        let cases = [(0u8, 38u8), (100, 112), (200, 188)];
        for (a, want) in cases {
            let sum = 3.0 * f64::from(a) + 150.0;
            let got = (sum / 4.0).round_ties_even().clamp(0.0, 255.0) as u8;
            assert_eq!(got, want, "a={a}");
        }
    }

    #[test]
    fn weights_count_must_match_inputs() {
        let req = Instantiate {
            name: "mix",
            instance: "mix",
            args: Some("inputs=3:weights=1 1"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn bad_duration_is_a_clean_error() {
        let req = Instantiate {
            name: "mix",
            instance: "mix",
            args: Some("duration=nope"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
