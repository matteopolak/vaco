//! `blend` — combine two video inputs, per pixel, with a named blend-mode
//! function per colour component.
//!
//! `ffmpeg -h filter=blend` (2026-08-28): `c0_mode`..`c3_mode` (`0..=39`,
//! default `normal`, 40 named values with several aliasing pairs — e.g.
//! `addition128`/`grainmerge` both name `28`), `c0_expr`..`c3_expr` and
//! `all_expr` (arbitrary per-component expressions), `c0_opacity`..
//! `c3_opacity` and `all_opacity` (`0..=1`, default `1`), plus the full
//! `vaco-filter-framesync` surface (`eof_action`/`shortest`/`repeatlast`/
//! `ts_sync_mode`) — measured directly, this is *not* the reduced surface
//! `hstack`/`vstack` expose; `blend` is architecturally the same shape as
//! `overlay`.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, a `0..=255` gradient against a
//! fixed second operand, hand-built `rawvideo` sources)
//!
//! For each mode below, `a` is the first input's sample, `b` the second's
//! — pinned at 6 points across the full gradient (`a = 0, 50, 100, 150,
//! 200, 255`, `b = 150` fixed), with a second fixed-`b` sweep wherever the
//! first could plausibly hide an argument-order or rounding ambiguity:
//!
//! ```text
//! normal(a, b)   = a
//! multiply(a, b) = floor(a*b / 255)
//! screen(a, b)   = 255 - floor((255-a)*(255-b) / 255)
//! darken(a, b)   = min(a, b)
//! lighten(a, b)  = max(a, b)
//! average(a, b)  = floor((a+b) / 2)
//! difference(a,b)= |a - b|
//! subtract(a, b) = max(0, a - b)
//! addition(a, b) = min(255, a + b)
//! exclusion(a,b) = a + b - floor(2*a*b / 255)
//! negation(a, b) = 255 - |255 - a - b|
//! grainmerge(a,b)  (= addition128)   = clamp(a + b - 128, 0, 255)
//! grainextract(a,b)(= difference128) = clamp(a - b + 128, 0, 255)
//! and(a, b) = a & b   (bitwise, exact)
//! or(a, b)  = a | b   (bitwise, exact)
//! xor(a, b) = a ^ b   (bitwise, exact)
//! ```
//!
//! `burn`/`dodge` are the one pair that measurably use **round-half-up**
//! on their division, not the `floor` every fixed-`/255` formula above
//! uses — confirmed by an exact `.5` tie (`a=150, b=150` inside `burn`'s
//! division lands on `178.5`, and the reference's output is only
//! consistent with that rounding to `179`, not `178`):
//!
//! ```text
//! burn(a, b)  = if a == 0 { 0 } else { clamp(255 - round((255-b)*255/a), 0, 255) }
//! dodge(a, b) = if a == 255 { 255 } else { clamp(round(b*255/(255-a)), 0, 255) }
//! ```
//!
//! `opacity` (default `1`, a no-op) blends the mode's own output back
//! toward `a`: measured directly at `opacity=0.5` against `multiply`,
//! `out = floor(a + opacity*(mode(a,b) - a))` — confirmed against 6
//! points, all matching the `floor`, not `round`, convention.
//!
//! # Not measured/implemented
//!
//! `hardlight`, `overlay`, `softlight`, `hardmix`, `linearlight`,
//! `vividlight`, `pinlight`, `reflect`, `phoenix`, `extremity`, `freeze`,
//! `glow`, `heat`, `softdifference`, `geometric`, `harmonic`, `bleach`,
//! `stain`, `interpolate`, `hardoverlay`, `multiply128`: raw output
//! curves were captured (see `docs/filter/vaco-filter-overlay.md`) but no
//! single-point-then-confirm formula was found with confidence.
//!
//! A second, bounded attempt was made at `hardlight`/`vividlight`/
//! `linearlight` specifically, after `burn`/`dodge` turned up
//! round-half-up: these three are the ones a published W3C-style formula
//! would build from `multiply`/`screen` (a threshold at `127`/`128`) or
//! from `burn`/`dodge` themselves, so the new rounding rule was the most
//! promising lead available. It did not unlock them. Sweeping `hardlight`
//! at two fixed second operands (`b=60`, `b=200`) against the standard
//! `a<=127 -> multiply(b, 2a)` / `a>127 -> screen(b, 2a-255)` shape shows
//! the *large*-`a` end matching a plain `floor(a*2b/255)` exactly
//! (`a=150,200,255` all exact against `b=60`), while the small-`a` end is
//! consistently one below that prediction (`a=50,100`) — neither `floor`
//! nor `round`, nor dividing by `256` instead of `255`, reconciles both
//! ends with one rule. That specific, falsified shape is recorded here
//! so a future attempt does not re-derive and re-reject the same three
//! candidates. `create` rejects all twenty modes with a clean error
//! rather than a guess. `c0_expr`/`all_expr` (arbitrary expressions) are
//! not implemented. Bit depths above 8.

use vaco_core::{MediaType, Result};
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
        name: "top",
        media_type: MediaType::Video,
    },
    Pad {
        name: "bottom",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "blend",
    description: "Blend two video frames into each other.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

/// One named blend-mode formula. `Normal` is `0`, matching the reference's
/// own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Multiply,
    Screen,
    Darken,
    Lighten,
    Average,
    Difference,
    Subtract,
    Addition,
    Burn,
    Dodge,
    Exclusion,
    And,
    Or,
    Xor,
    Negation,
    GrainMerge,
    GrainExtract,
}

impl Mode {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "multiply" => Some(Self::Multiply),
            "screen" => Some(Self::Screen),
            "darken" => Some(Self::Darken),
            "lighten" => Some(Self::Lighten),
            "average" => Some(Self::Average),
            "difference" => Some(Self::Difference),
            "subtract" => Some(Self::Subtract),
            "addition" => Some(Self::Addition),
            "burn" => Some(Self::Burn),
            "dodge" => Some(Self::Dodge),
            "exclusion" => Some(Self::Exclusion),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "xor" => Some(Self::Xor),
            "negation" => Some(Self::Negation),
            "grainmerge" | "addition128" => Some(Self::GrainMerge),
            "grainextract" | "difference128" => Some(Self::GrainExtract),
            _ => None,
        }
    }

    /// Apply this mode to one byte pair. See the module doc for every
    /// formula and its measurement.
    #[must_use]
    pub(crate) fn apply(self, a: u8, b: u8) -> u8 {
        match self {
            Self::And => a & b,
            Self::Or => a | b,
            Self::Xor => a ^ b,
            _ => {
                let (ai, bi) = (i32::from(a), i32::from(b));
                match self {
                    Self::Normal => common::clamp_u8(ai),
                    Self::Multiply => {
                        #[allow(
                            clippy::integer_division,
                            reason = "a*b/255 is an exact floor by construction (fixed-point 8-bit blend math), not a lossy average"
                        )]
                        {
                            common::clamp_u8(ai * bi / 255)
                        }
                    }
                    Self::Screen => {
                        #[allow(
                            clippy::integer_division,
                            reason = "the inner product/255 is an exact floor by construction, not a lossy average"
                        )]
                        {
                            common::clamp_u8(255 - (255 - ai) * (255 - bi) / 255)
                        }
                    }
                    Self::Darken => common::clamp_u8(ai.min(bi)),
                    Self::Lighten => common::clamp_u8(ai.max(bi)),
                    Self::Average => common::clamp_u8(i32::midpoint(ai, bi)),
                    Self::Difference => common::clamp_u8((ai - bi).abs()),
                    Self::Subtract => common::clamp_u8(ai - bi),
                    Self::Addition => common::clamp_u8(ai + bi),
                    Self::Exclusion => {
                        #[allow(
                            clippy::integer_division,
                            reason = "2*a*b/255 is an exact floor by construction, not a lossy average"
                        )]
                        {
                            common::clamp_u8(ai + bi - (2 * ai * bi) / 255)
                        }
                    }
                    Self::Negation => common::clamp_u8(255 - (255 - ai - bi).abs()),
                    Self::GrainMerge => common::clamp_u8(ai + bi - 128),
                    Self::GrainExtract => common::clamp_u8(ai - bi + 128),
                    Self::Burn => {
                        if ai == 0 {
                            0
                        } else {
                            common::clamp_u8(255 - round_div((255 - bi) * 255, ai))
                        }
                    }
                    Self::Dodge => {
                        if ai == 255 {
                            255
                        } else {
                            common::clamp_u8(round_div(bi * 255, 255 - ai))
                        }
                    }
                    Self::And | Self::Or | Self::Xor => unreachable!("handled above"),
                }
            }
        }
    }
}

/// `round(num/den)` for non-negative `num`/`den` — round-half-up via the
/// integer "add half the divisor, then floor-divide" trick, matching
/// `burn`/`dodge`'s measured rounding (see the module doc's exact-tie
/// probe). `den == 0` is each mode's own special case, never reached here.
fn round_div(num: i32, den: i32) -> i32 {
    if den <= 0 {
        return i32::MAX;
    }
    #[allow(
        clippy::integer_division,
        reason = "add-half-then-floor is the intended round-half-up trick, not a lossy average"
    )]
    {
        (num + den / 2) / den
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "blend", help = "Blend two video frames into each other.")]
pub(crate) struct Opts {
    #[opt(name = "all_mode", help = "set blend mode for all components", default = String::new(), flags(video, filtering))]
    pub all_mode: String,
    #[opt(name = "c0_mode", help = "set blend mode for component #0", default = "normal".to_owned(), flags(video, filtering))]
    pub c0_mode: String,
    #[opt(name = "c1_mode", help = "set blend mode for component #1", default = "normal".to_owned(), flags(video, filtering))]
    pub c1_mode: String,
    #[opt(name = "c2_mode", help = "set blend mode for component #2", default = "normal".to_owned(), flags(video, filtering))]
    pub c2_mode: String,
    #[opt(name = "c3_mode", help = "set blend mode for component #3", default = "normal".to_owned(), flags(video, filtering))]
    pub c3_mode: String,
    /// `-1` is this crate's own sentinel for "not explicitly given" — the
    /// reference's own default (`1`) is a legitimate value a user can also
    /// set explicitly, so it cannot double as the sentinel.
    #[opt(name = "all_opacity", help = "set opacity for all components", default = -1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub all_opacity: f64,
    #[opt(name = "c0_opacity", help = "set opacity for component #0", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub c0_opacity: f64,
    #[opt(name = "c1_opacity", help = "set opacity for component #1", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub c1_opacity: f64,
    #[opt(name = "c2_opacity", help = "set opacity for component #2", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub c2_opacity: f64,
    #[opt(name = "c3_opacity", help = "set opacity for component #3", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub c3_opacity: f64,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
    pub shortest: bool,
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
    modes: [Mode; 4],
    opacities: [f64; 4],
    shortest: bool,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts {
            shortest: self.shortest,
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
        let Some(top) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = top.data
        else {
            return Ok(FrameOut::One(top));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(top));
        }
        let Some(bottom) = event.get(1) else {
            return Ok(FrameOut::One(top));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count.min(4) {
            let mode = self.modes.get(plane).copied().unwrap_or(Mode::Normal);
            let opacity = self.opacities.get(plane).copied().unwrap_or(1.0);
            let Some(a_plane) = top.plane(plane) else {
                continue;
            };
            let Some(b_plane) = bottom.plane(plane) else {
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
                // `normal` always returns the first input, and opacity blends
                // that value back toward the same first input. A span copy is
                // therefore exact for every valid opacity, including a short
                // malformed row where only `n` bytes are addressable.
                if mode == Mode::Normal {
                    let (Some(src), Some(dst)) = (a_row.get(..n), dst_row.get_mut(..n)) else {
                        continue;
                    };
                    dst.copy_from_slice(src);
                    continue;
                }
                for x in 0..n {
                    let (Some(&a), Some(&b)) = (a_row.get(x), b_row.get(x)) else {
                        continue;
                    };
                    let blended = mode.apply(a, b);
                    #[allow(clippy::cast_precision_loss, reason = "8-bit samples fit f64 exactly")]
                    let out_val = if (opacity - 1.0).abs() < f64::EPSILON {
                        blended
                    } else {
                        let mixed = f64::from(a) + opacity * (f64::from(blended) - f64::from(a));
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "mixed is a convex combination of two bytes, clamped"
                        )]
                        {
                            mixed.floor().clamp(0.0, 255.0) as u8
                        }
                    };
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
    let mode_names = if opts.all_mode.is_empty() {
        [
            opts.c0_mode.clone(),
            opts.c1_mode.clone(),
            opts.c2_mode.clone(),
            opts.c3_mode.clone(),
        ]
    } else {
        [
            opts.all_mode.clone(),
            opts.all_mode.clone(),
            opts.all_mode.clone(),
            opts.all_mode.clone(),
        ]
    };
    let mut modes = [Mode::Normal; 4];
    for (i, name) in mode_names.iter().enumerate() {
        let mode = Mode::from_name(name)
            .ok_or_else(|| format!("blend: mode `{name}` is not implemented"))?;
        if let Some(slot) = modes.get_mut(i) {
            *slot = mode;
        }
    }
    let opacities = if opts.all_opacity >= 0.0 {
        [opts.all_opacity; 4]
    } else {
        [
            opts.c0_opacity,
            opts.c1_opacity,
            opts.c2_opacity,
            opts.c3_opacity,
        ]
    };
    let filter = Filter {
        modes,
        opacities,
        shortest: opts.shortest,
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

    /// Pinned against the reference probe in this module's doc: a full
    /// `0..=255` gradient against a fixed `b=150`.
    #[test]
    fn arithmetic_modes_match_the_reference_gradient_probe() {
        let cases: &[(Mode, [u8; 6])] = &[
            (Mode::Multiply, [0, 29, 58, 88, 117, 150]),
            (Mode::Screen, [150, 171, 192, 212, 233, 255]),
            (Mode::Darken, [0, 50, 100, 150, 150, 150]),
            (Mode::Lighten, [150, 150, 150, 150, 200, 255]),
            (Mode::Average, [75, 100, 125, 150, 175, 202]),
            (Mode::Addition, [150, 200, 250, 255, 255, 255]),
            (Mode::Difference, [150, 100, 50, 0, 50, 105]),
            (Mode::Subtract, [0, 0, 0, 0, 50, 105]),
            (Mode::Exclusion, [150, 142, 133, 124, 115, 105]),
            (Mode::Negation, [150, 200, 250, 210, 160, 105]),
            (Mode::GrainMerge, [22, 72, 122, 172, 222, 255]),
            (Mode::GrainExtract, [0, 28, 78, 128, 178, 233]),
            (Mode::And, [0, 18, 4, 150, 128, 150]),
            (Mode::Or, [150, 182, 246, 150, 222, 255]),
            (Mode::Xor, [150, 164, 242, 0, 94, 105]),
        ];
        let gradient = [0u8, 50, 100, 150, 200, 255];
        for (mode, expected) in cases {
            for (a, &want) in gradient.iter().zip(expected.iter()) {
                assert_eq!(mode.apply(*a, 150), want, "{mode:?} at a={a}");
            }
        }
    }

    /// Pinned: `normal` passes the first operand through unchanged.
    #[test]
    fn normal_is_the_first_operand() {
        for a in [0u8, 50, 150, 255] {
            assert_eq!(Mode::Normal.apply(a, 150), a);
        }
    }

    /// Pinned against the reference's exact-tie probe: `burn`/`dodge`
    /// round half away from zero, not floor — the one place this
    /// module's formulas diverge from the fixed-`/255` `floor` rule.
    #[test]
    fn burn_and_dodge_round_half_up_at_an_exact_tie() {
        // burn(150, 150): (255-150)*255/150 = 178.5 exactly.
        assert_eq!(Mode::Burn.apply(150, 150), 76);
        assert_eq!(Mode::Burn.apply(200, 150), 121);
        assert_eq!(Mode::Burn.apply(0, 150), 0);
        assert_eq!(Mode::Burn.apply(255, 150), 150);
        assert_eq!(Mode::Dodge.apply(50, 150), 187);
        assert_eq!(Mode::Dodge.apply(100, 150), 247);
        assert_eq!(Mode::Dodge.apply(255, 150), 255);
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "blend",
            instance: "blend",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn unimplemented_mode_is_a_clean_error() {
        let req = Instantiate {
            name: "blend",
            instance: "blend",
            args: Some("all_mode=hardlight"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference probe: `opacity=0.5` on `multiply`
    /// floors the convex combination.
    #[test]
    fn opacity_blends_toward_the_first_operand_and_floors() {
        let gradient = [0u8, 50, 100, 150, 200, 255];
        let expected = [0u8, 39, 79, 119, 158, 202];
        for (a, &want) in gradient.iter().zip(expected.iter()) {
            let blended = Mode::Multiply.apply(*a, 150);
            let mixed = f64::from(*a) + 0.5 * (f64::from(blended) - f64::from(*a));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let got = mixed.floor().clamp(0.0, 255.0) as u8;
            assert_eq!(got, want, "a={a}");
        }
    }
}
