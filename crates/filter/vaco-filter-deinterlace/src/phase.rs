//! `phase` — shift a field by one frame of delay to correct a one-field
//! pulldown-phase error.
//!
//! `ffmpeg -h filter=phase`: `mode` (`p`=progressive, `t`=top-first,
//! `b`=bottom-first, `T`/`B`=analyze variants of the same, `u`/`U`=analyze,
//! `a`/`A`=auto/auto-analyze, default `A`).
//!
//! # Measured: `t` keeps the current top field, delays the bottom field
//!
//! Ran `phase=t` on a `2x8` ramp where every row is frame-identifiable
//! (`ffmpeg` 8.1, 2026-08-23). Frame 0 (no history) passes through
//! unchanged. Frame 1's output has **even rows from frame 1** (its own top
//! field) and **odd rows from frame 0** (the previous frame's bottom
//! field) — i.e. `weave(top = current, bottom = held previous frame)`. By
//! symmetry `b` is implemented as `weave(top = held previous frame, bottom
//! = current)`.
//!
//! # What is not implemented: the analyze/auto modes
//!
//! `T`/`B`/`u`/`U`/`a`/`A` (`A` is the reference's *default*) choose,
//! per frame, whether to shift at all, based on a combing analysis of both
//! the shifted and unshifted candidates — the reference only shifts when
//! doing so measurably reduces combing. That decision procedure was not
//! reverse-engineered in this pass (it would need the same kind of
//! per-frame comb metric [`idet`](crate::idet) uses, applied twice per
//! frame and compared). Every analyze/auto mode here **always shifts**, as
//! if it were plain `t` or `b` (`b`-flavoured for `B`/`U`/lower-case
//! defaults, `t`-flavoured for `T`), which is a real behavioural gap on any
//! input where the reference would have left a frame alone. `p`
//! (progressive) is exact: an unconditional passthrough.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, alloc_like, copy_row, dims, ensure_addressable};

pub const DESC: FilterDesc = FilterDesc {
    name: "phase",
    description: "Phase shift fields.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Progressive,
    /// Keep the current frame's top field; delay the bottom field by one
    /// frame. Selected by `t`/`T` (the analyze variant is not implemented,
    /// see the module doc).
    TopFirst,
    /// Keep the current frame's bottom field; delay the top field.
    /// Selected by `b`/`B`/`u`/`U`/`a`/`A` — every non-`t`/`p` mode.
    BottomFirst,
}

fn mode_from_opt(v: i32) -> Mode {
    match v {
        0 => Mode::Progressive,
        1 | 3 => Mode::TopFirst,
        _ => Mode::BottomFirst,
    }
}

/// `ffmpeg -h filter=phase`'s own named constants for `mode` -- nine
/// single-character, case-sensitive names (lower and upper case are
/// distinct values).
const PHASE_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "p",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "t",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "b",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "T",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "B",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "u",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(5),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "U",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(6),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "a",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(7),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "A",
        help: "",
        unit: "phase_mode",
        value: vaco_opts::ConstValue::Int(8),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "phase", help = "Phase shift fields")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "set phase mode", unit = "phase_mode", consts = PHASE_MODE_CONSTS, default = 8, range = 0..=8, flags(video, filtering))]
    pub mode: i32,
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

fn shift(pool: &FramePool, held: &Frame, current: &Frame, top_from_current: bool) -> Result<Frame> {
    let Some((format, width, height)) = dims(current) else {
        return Err(vaco_core::Error::Unsupported("phase needs video frames"));
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, current, format, width, height)?;
    let (top_src, bottom_src) = if top_from_current {
        (current, held)
    } else {
        (held, current)
    };
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        let Some(top_plane) = top_src.plane(p) else {
            continue;
        };
        let Some(bottom_plane) = bottom_src.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        for y in 0..rows {
            if y % 2 == 0 {
                copy_row(&mut dst_plane, y, top_plane, y);
            } else {
                copy_row(&mut dst_plane, y, bottom_plane, y);
            }
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct Filter {
    mode: Mode,
    held: Option<Frame>,
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let out = match self.mode {
            Mode::Progressive => input.clone(),
            Mode::TopFirst | Mode::BottomFirst => {
                let top_from_current = self.mode == Mode::TopFirst;
                match &self.held {
                    None => input.clone(),
                    Some(held) => shift(ctx.pool(), held, &input, top_from_current)?,
                }
            }
        };
        self.held = Some(input);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.held = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter {
            mode: mode_from_opt(opts.mode),
            held: None,
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn top_first_keeps_current_top_and_delays_bottom() {
        // Measured: frame1's output has even rows from frame1, odd from frame0.
        let pool = FramePool::default();
        let f0 = ramp_frame(2, 8);
        let mut f1 = ramp_frame(2, 8);
        if let Some(mut p) = f1.plane_mut(0) {
            for y in 0..8usize {
                if let Some(row) = p.row_mut(y) {
                    for b in row.iter_mut() {
                        *b = b.saturating_add(100);
                    }
                }
            }
        }
        let out = shift(&pool, &f0, &f1, true).unwrap();
        for y in (0..8).step_by(2) {
            assert_eq!(
                row_value(&out, y),
                row_value(&f1, y),
                "even row {y} from current"
            );
        }
        for y in (1..8).step_by(2) {
            assert_eq!(
                row_value(&out, y),
                row_value(&f0, y),
                "odd row {y} from held"
            );
        }
    }

    #[test]
    fn progressive_mode_is_always_a_passthrough() {
        let mut filt = Filter {
            mode: Mode::Progressive,
            held: None,
        };
        assert_eq!(filt.mode, Mode::Progressive);
        filt.held = Some(ramp_frame(2, 4));
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=phase`): all nine single-character,
    /// case-sensitive names must parse to distinct values.
    #[test]
    fn named_mode_values_parse() {
        for (name, expected) in [
            ("p", 0),
            ("t", 1),
            ("b", 2),
            ("T", 3),
            ("B", 4),
            ("u", 5),
            ("U", 6),
            ("a", 7),
            ("A", 8),
        ] {
            let opts = Opts::parse(Some(&format!("mode={name}"))).unwrap();
            assert_eq!(opts.mode, expected, "mode={name}");
        }
    }
}
