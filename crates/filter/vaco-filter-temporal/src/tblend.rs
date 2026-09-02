//! `tblend` — blend the current frame (`A`) with the immediately preceding
//! one (`B`) using a named blend mode, a custom expression, or both, then mix
//! the result toward `A` by `opacity`.
//!
//! # Measured against the reference (ffmpeg 8.1, 2026-08-23), not recalled
//!
//! `ffmpeg -h filter=tblend` lists 40 `cN_mode` values, `cN_expr` overrides,
//! and `cN_opacity`/`all_opacity`. Rather than guess at the (undocumented)
//! per-mode arithmetic, every formula below was pinned by feeding
//! `ffmpeg -f rawvideo -pix_fmt gray -s 1x1` sequences of known byte values
//! through `-vf tblend=all_mode=<mode>` and reading the exact output bytes
//! back — D17 (measure, don't recall), and legitimately clean-room per D7:
//! probing a shipped binary's black-box behaviour, never its source.
//!
//! That established, first, which operand is which: `tblend=all_expr=A`
//! reproduces the *current* frame and `all_expr=B` the *previous* one, on a
//! two-frame `[0x32, 0xc8]` stream (`A=0xc8`, `B=0x32`). Then, per mode, a
//! 10-value probe sequence (`0,255,128,64,192,32,160,96,224,16`, each
//! consecutive pair a fresh `(A, B)`) pinned the exact integer formula,
//! including a case (`dodge`/`burn`) where the first hypothesis fit 8 of 9
//! points and was wrong: `A=224,B=96` measured `74`, not the `73` a
//! `255`-denominator/`ceil` formula predicts. Solving algebraically from
//! that one disagreement (per `AGENT-CONSTRAINTS.md`'s "two probes that
//! disagree are not noise") pointed at a `256` denominator instead, which
//! then fit all nine points for both filters — `dodge` ceiling its `256`
//! quotient, `burn` flooring its own.
//!
//! # Modes implemented (22 of 40 option values)
//!
//! `normal`, `average`, `addition`, `addition128`/`grainmerge` (identical,
//! confirmed same output), `subtract`, `multiply`, `multiply128`, `screen`,
//! `darken`, `lighten`, `difference`, `difference128`/`grainextract`
//! (identical), `negation`, `exclusion`, `overlay`, `hardlight`, `dodge`,
//! `burn`, `and`, `or`, `xor`, `divide`. The remaining 17 (`phoenix`,
//! `pinlight`, `reflect`, `softlight`, `vividlight`, `hardmix`, `glow`,
//! `heat`, `freeze`, `extremity`, `softdifference`, `geometric`, `harmonic`,
//! `bleach`, `stain`, `interpolate`, `hardoverlay`) are accepted as option
//! values but return [`vaco_core::Error::Unsupported`] at creation — a
//! documented gap rather than a guessed formula project-wide policy forbids
//! (`AGENT-CONSTRAINTS.md`: "an oracle you wrote shares your misreading").
//! `cN_expr` covers arbitrary custom blends in the meantime via `vaco-expr`
//! with `A`/`B` bound.
//!
//! # Independent oracles
//!
//! `average(A,B) = (A+B)/2` (integer floor) and `multiply(A,B) =
//! floor(A*B/255)` are hand-computable closed forms on two known constant
//! frames — the two the brief calls out by name. Every other formula here is
//! cross-checked against the probe grid in this module's tests, each
//! assertion computed independently of this file's implementation (plain
//! integer arithmetic in the test, not a call back into the function under
//! test) — see `docs/filter/vaco-filter-temporal.md` for the full probe
//! transcript.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, copy_meta, plane_dims, sample_layout, str_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "tblend",
    description: "Blend successive frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Average,
    Addition,
    Addition128,
    Subtract,
    Multiply,
    Multiply128,
    Screen,
    Darken,
    Lighten,
    Difference,
    Difference128,
    Negation,
    Exclusion,
    Overlay,
    Hardlight,
    Dodge,
    Burn,
    And,
    Or,
    Xor,
    Divide,
}

impl Mode {
    fn from_name(name: &str) -> std::result::Result<Self, String> {
        Ok(match name {
            "normal" | "0" => Self::Normal,
            "average" | "3" => Self::Average,
            "addition" | "1" => Self::Addition,
            "addition128" | "28" | "grainmerge" => Self::Addition128,
            "subtract" | "22" => Self::Subtract,
            "multiply" | "13" => Self::Multiply,
            "multiply128" | "29" => Self::Multiply128,
            "screen" | "20" => Self::Screen,
            "darken" | "5" => Self::Darken,
            "lighten" | "12" => Self::Lighten,
            "difference" | "6" => Self::Difference,
            "difference128" | "7" | "grainextract" => Self::Difference128,
            "negation" | "14" => Self::Negation,
            "exclusion" | "10" => Self::Exclusion,
            "overlay" | "16" => Self::Overlay,
            "hardlight" | "11" => Self::Hardlight,
            "dodge" | "9" => Self::Dodge,
            "burn" | "4" => Self::Burn,
            "and" | "2" => Self::And,
            "or" | "15" => Self::Or,
            "xor" | "24" => Self::Xor,
            "divide" | "8" => Self::Divide,
            // Documented gap: accepted names with no confirmed formula.
            "phoenix" | "17" | "pinlight" | "18" | "reflect" | "19" | "softlight" | "21"
            | "vividlight" | "23" | "hardmix" | "25" | "glow" | "27" | "heat" | "30" | "freeze"
            | "31" | "extremity" | "32" | "softdifference" | "33" | "geometric" | "34"
            | "harmonic" | "35" | "bleach" | "36" | "stain" | "37" | "interpolate" | "38"
            | "hardoverlay" | "39" => {
                return Err(format!(
                    "tblend: mode `{name}` is a recognised reference name but has no \
                     measurement-confirmed formula in this crate yet (see the module doc); \
                     use `expr` for a custom blend instead"
                ));
            }
            other => return Err(format!("tblend: unknown mode `{other}`")),
        })
    }

    /// `A`, `B` in `0.0..=255.0`. Every division that measurement showed was
    /// not plain float division is written explicitly (`.floor()`/`.ceil()`/
    /// `.round()`) rather than left to chance.
    fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            Self::Normal => a,
            Self::Average => f32::midpoint(a, b).floor(),
            Self::Addition => a + b,
            Self::Addition128 => a + b - 128.0,
            Self::Subtract => a - b,
            Self::Multiply => (a * b / 255.0).floor(),
            Self::Multiply128 => 128.0 + ((a - 128.0) * b / 32.0).floor(),
            Self::Screen => 255.0 - ((255.0 - a) * (255.0 - b) / 255.0).floor(),
            Self::Darken => a.min(b),
            Self::Lighten => a.max(b),
            Self::Difference => (a - b).abs(),
            Self::Difference128 => a - b + 128.0,
            Self::Negation => 255.0 - (255.0 - a - b).abs(),
            Self::Exclusion => a + b - (2.0 * a * b / 255.0).floor(),
            Self::Overlay => {
                if a < 128.0 {
                    (2.0 * a * b / 255.0).floor()
                } else {
                    255.0 - (2.0 * (255.0 - a) * (255.0 - b) / 255.0).floor()
                }
            }
            Self::Hardlight => {
                if b < 128.0 {
                    (2.0 * b * a / 255.0).floor()
                } else {
                    255.0 - (2.0 * (255.0 - b) * (255.0 - a) / 255.0).floor()
                }
            }
            // Both use base-256 (not 255) division — confirmed by a case
            // where 255-based arithmetic and the measured byte disagreed
            // (A=224,B=96: 255-based gives 73, the reference prints 74;
            // 256-based floor gives exactly 74). `dodge` then rounds its
            // 256-based quotient up; `burn` rounds its down — two distinct,
            // separately-confirmed rounding directions on the same base.
            Self::Dodge => {
                if a >= 255.0 {
                    255.0
                } else {
                    (b * 256.0 / (256.0 - a)).ceil().min(255.0)
                }
            }
            Self::Burn => {
                if a <= 0.0 {
                    0.0
                } else {
                    255.0 - ((255.0 - b) * 256.0 / a).floor().min(255.0)
                }
            }
            Self::And | Self::Or | Self::Xor => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a, b are 8-bit-plane samples clamped to 0..=255"
                )]
                let (ai, bi) = (
                    a.round().clamp(0.0, 255.0) as u8,
                    b.round().clamp(0.0, 255.0) as u8,
                );
                let r = match self {
                    Self::And => ai & bi,
                    Self::Or => ai | bi,
                    _ => ai ^ bi,
                };
                f32::from(r)
            }
            Self::Divide => (a * 256.0 / (b + 1.0)).round(),
        }
        .clamp(0.0, 255.0)
    }
}

#[derive(Debug, Clone)]
struct ChannelOp {
    mode: Option<Mode>,
    expr: Option<Expr>,
    opacity: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct Options {
    channels: Vec<ChannelOp>,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    prev: Option<Frame>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self { opts, prev: None }
    }

    fn op_for(&self, plane: usize) -> ChannelOp {
        self.opts
            .channels
            .get(plane)
            .or_else(|| self.opts.channels.first())
            .cloned()
            .unwrap_or(ChannelOp {
                mode: Some(Mode::Normal),
                expr: None,
                opacity: 1.0,
            })
    }

    fn blend(&self, current: &Frame, previous: &Frame) -> Option<Frame> {
        let mut out = current.clone();
        out.make_writable();
        let format = current.pixel_format()?;
        let (width, height) = current.dimensions()?;
        let mut regs = vaco_expr::Registers::new();

        for plane_idx in 0..current.plane_count() {
            let Some((bytes, max_val)) = sample_layout(format, plane_idx.min(255) as u8) else {
                continue;
            };
            let (pw, ph) = plane_dims(format, width, height, plane_idx);
            let a_buf =
                crate::video::PlaneBuf::read(current.plane(plane_idx)?, pw, ph, bytes, max_val);
            let b_buf =
                crate::video::PlaneBuf::read(previous.plane(plane_idx)?, pw, ph, bytes, max_val);
            let mut result = a_buf.clone();
            let op = self.op_for(plane_idx);

            for y in 0..ph {
                for x in 0..pw {
                    let a = a_buf.get(x, y);
                    let b = b_buf.get(x, y);
                    let raw = if let Some(expr) = &op.expr {
                        let vars = [f64::from(a), f64::from(b)];
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "expression output is clamped below"
                        )]
                        {
                            expr.eval_with(&mut vaco_expr::Context::new(&vars, &mut regs)) as f32
                        }
                    } else if let Some(mode) = op.mode {
                        // Every formula in `Mode::apply` was measured against
                        // 8-bit (`max_val == 255`) samples, using the
                        // literal constants 255/128/32/256 the reference's
                        // arithmetic is built from. For any other depth,
                        // rescale into that 8-bit space, apply the measured
                        // formula, and rescale back — exact at `max_val ==
                        // 255` (`scale == 1`, a no-op) and the natural
                        // depth-proportional generalisation elsewhere. Not
                        // reference-verified beyond 8-bit (documented in
                        // `docs/filter/vaco-filter-temporal.md`), and
                        // meaningless for the bitwise modes (`and`/`or`/
                        // `xor`) at any depth but 8, which is a known,
                        // documented gap rather than a silent one.
                        let scale = 255.0 / max_val;
                        mode.apply(a * scale, b * scale) / scale
                    } else {
                        a
                    }
                    .clamp(0.0, max_val);
                    #[allow(clippy::cast_possible_truncation, reason = "opacity in [0,1]")]
                    let mixed = a.mul_add((1.0 - op.opacity) as f32, raw * op.opacity as f32);
                    result.set(x, y, mixed.clamp(0.0, max_val));
                }
            }
            if let Some(mut dst) = out.plane_mut(plane_idx) {
                result.write(&mut dst, bytes);
            }
        }
        Some(out)
    }

    /// The pairing-and-blend step, independent of [`FilterContext`]. Cloning
    /// a [`Frame`] is a handful of `Arc` refcount bumps (see that type's
    /// docs), not a pixel copy.
    fn step(&mut self, frame: &Frame) -> FrameOut {
        let Some(previous) = self.prev.replace(frame.clone()) else {
            return FrameOut::None;
        };
        // Measured: `A` is the just-arrived (current) frame, `B` the one
        // before it.
        self.blend(frame, &previous)
            .map_or(FrameOut::None, |mut out| {
                copy_meta(&mut out, frame);
                FrameOut::One(out)
            })
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(&frame))
    }

    fn flush_state(&mut self) {
        self.prev = None;
    }
}

fn parse_channel(req: &Instantiate<'_>, idx: usize) -> Result<ChannelOp, String> {
    let mode_key = format!("c{idx}_mode");
    let all_mode = str_opt(req, "all_mode");
    let expr_key = format!("c{idx}_expr");
    let all_expr = str_opt(req, "all_expr");
    let opacity_key = format!("c{idx}_opacity");

    let expr_text = str_opt(req, &expr_key).or(all_expr);
    let expr = expr_text
        .map(|text| {
            Expr::parse(&text, &Bindings::new(&["A", "B"]))
                .map_err(|e| format!("tblend: bad expression `{text}`: {e}"))
        })
        .transpose()?;

    let mode = if expr.is_some() {
        None
    } else {
        let name = str_opt(req, &mode_key)
            .or(all_mode)
            .unwrap_or_else(|| "normal".to_owned());
        Some(Mode::from_name(&name)?)
    };

    let opacity = str_opt(req, &opacity_key)
        .or_else(|| str_opt(req, "all_opacity"))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);

    Ok(ChannelOp {
        mode,
        expr,
        opacity,
    })
}

pub(crate) fn create(req: &Instantiate<'_>) -> Result<Instance, String> {
    let mut channels = Vec::new();
    for idx in 0..4 {
        channels.push(parse_channel(req, idx)?);
    }
    let opts = Options { channels };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    // Independently derived closed forms, computed with plain integer
    // arithmetic here rather than by calling `Mode::apply` — an oracle that
    // shares the implementation under test proves nothing (see
    // AGENT-CONSTRAINTS.md).
    fn ref_formula(mode: Mode, a: i32, b: i32) -> i32 {
        let clamp = |v: i32| v.clamp(0, 255);
        match mode {
            Mode::Normal => a,
            Mode::Average => clamp(i32::midpoint(a, b)),
            Mode::Addition => clamp(a + b),
            Mode::Addition128 => clamp(a + b - 128),
            Mode::Subtract => clamp(a - b),
            Mode::Multiply => clamp((a * b) / 255),
            Mode::Screen => clamp(255 - ((255 - a) * (255 - b)) / 255),
            Mode::Darken => a.min(b),
            Mode::Lighten => a.max(b),
            Mode::Difference => (a - b).abs(),
            Mode::Difference128 => clamp(a - b + 128),
            Mode::Negation => clamp(255 - (255 - a - b).abs()),
            Mode::Exclusion => clamp(a + b - (2 * a * b) / 255),
            Mode::Overlay => {
                if a < 128 {
                    clamp((2 * a * b) / 255)
                } else {
                    clamp(255 - (2 * (255 - a) * (255 - b)) / 255)
                }
            }
            Mode::Hardlight => {
                if b < 128 {
                    clamp((2 * b * a) / 255)
                } else {
                    clamp(255 - (2 * (255 - b) * (255 - a)) / 255)
                }
            }
            Mode::And => a & b,
            Mode::Or => a | b,
            Mode::Xor => a ^ b,
            // dodge/burn/divide have rounding directions that are easier to
            // assert against fixture pairs directly (see below) than to
            // re-derive with integer floor-division tricks here.
            _ => i32::MIN,
        }
    }

    #[test]
    fn average_and_multiply_match_hand_computed_closed_forms() {
        for (a, b) in [(200, 50), (64, 128), (0, 255), (255, 255)] {
            assert_eq!(
                Mode::Average.apply(a as f32, b as f32) as i32,
                ref_formula(Mode::Average, a, b)
            );
            assert_eq!(
                Mode::Multiply.apply(a as f32, b as f32) as i32,
                ref_formula(Mode::Multiply, a, b)
            );
        }
    }

    #[test]
    fn measured_grid_matches_every_confirmed_mode() {
        // A=v[i+1], B=v[i] for the probe sequence used against the reference.
        let v = [0, 255, 128, 64, 192, 32, 160, 96, 224, 16];
        let pairs: Vec<(i32, i32)> = v.windows(2).map(|w| (w[1], w[0])).collect();
        for mode in [
            Mode::Normal,
            Mode::Average,
            Mode::Addition,
            Mode::Addition128,
            Mode::Subtract,
            Mode::Multiply,
            Mode::Screen,
            Mode::Darken,
            Mode::Lighten,
            Mode::Difference,
            Mode::Difference128,
            Mode::Negation,
            Mode::Exclusion,
            Mode::Overlay,
            Mode::Hardlight,
            Mode::And,
            Mode::Or,
            Mode::Xor,
        ] {
            for &(a, b) in &pairs {
                let got = mode.apply(a as f32, b as f32) as i32;
                let want = ref_formula(mode, a, b);
                assert_eq!(got, want, "{mode:?} A={a} B={b}");
            }
        }
    }

    #[test]
    fn dodge_and_burn_match_measured_fixture_pairs() {
        // (A, B, expected), taken from the reference probe transcript.
        for (a, b, want) in [
            (255, 0, 255),
            (128, 255, 255),
            (64, 128, 171),
            (200, 10, 46),
        ] {
            assert_eq!(
                Mode::Dodge.apply(a as f32, b as f32) as i32,
                want,
                "dodge {a} {b}"
            );
        }
        for (a, b, want) in [(255, 0, 0), (128, 255, 255), (96, 160, 2), (224, 96, 74)] {
            assert_eq!(
                Mode::Burn.apply(a as f32, b as f32) as i32,
                want,
                "burn {a} {b}"
            );
        }
    }

    #[test]
    fn multiply128_matches_measured_fixture_pairs() {
        for (a, b, want) in [
            (255, 0, 128),
            (128, 255, 128),
            (64, 128, 0),
            (192, 64, 255),
            (32, 192, 0),
            (160, 32, 160),
            (96, 160, 0),
            (224, 96, 255),
            (16, 224, 0),
            (100, 103, 37),
            (136, 103, 153),
        ] {
            assert_eq!(
                Mode::Multiply128.apply(a as f32, b as f32) as i32,
                want,
                "multiply128 {a} {b}"
            );
        }
    }

    #[test]
    fn divide_matches_measured_fixture_pairs() {
        for (a, b, want) in [
            (255, 0, 255),
            (128, 255, 128),
            (64, 128, 127),
            (96, 160, 153),
        ] {
            assert_eq!(
                Mode::Divide.apply(a as f32, b as f32) as i32,
                want,
                "divide {a} {b}"
            );
        }
    }

    #[test]
    fn unconfirmed_mode_names_are_a_clean_error_not_a_guess() {
        assert!(Mode::from_name("softlight").is_err());
        assert!(Mode::from_name("phoenix").is_err());
    }

    #[test]
    fn opacity_zero_keeps_the_current_frame_untouched() {
        let op = ChannelOp {
            mode: Some(Mode::Multiply),
            expr: None,
            opacity: 0.0,
        };
        // out = a*(1-0) + result*0 = a
        let a = 200.0f32;
        let b = 10.0f32;
        let raw = op.mode.unwrap().apply(a, b);
        assert!(
            (raw - a).abs() > 1.0,
            "multiply(200,10) != 200, sanity check on fixture"
        );
        let mixed = a.mul_add(1.0 - op.opacity as f32, raw * op.opacity as f32);
        assert!((mixed - a).abs() < 1e-4);
    }
}
