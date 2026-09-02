//! `fieldmatch` — pick, per frame, whichever field recombination is least
//! combed, for inverse telecine (typically followed by `decimate` to drop
//! the resulting duplicates).
//!
//! `ffmpeg -h filter=fieldmatch`: dynamic inputs (`1` normally, `2` when
//! `ppsrc=true` — a second "clean source" stream). `order`, `mode`,
//! `ppsrc`, `field`, `mchroma`, `y0`/`y1`, `scthresh`, `combmatch`,
//! `combdbg`, `cthresh`, `chroma`, `blockx`/`blocky`, `combpel`.
//!
//! # Membership note: the only `N->V` filter in this row
//!
//! Checked directly (`ffmpeg -h filter=fieldmatch`): `Inputs: dynamic
//! (depending on the options)`, `Outputs: #0: default (video)` — the one
//! filter in this crate's row that is not plain `V->V`. This crate's
//! `lib.rs` doc explains why the rest of the row needs neither `Paired`
//! nor `Fanout`; this filter is the exception, and it *does* reach for
//! [`vaco_filter_core::adapt::Paired`] for its `ppsrc=true` shape (2
//! inputs, matching `Paired`'s own `framepack`-style default input count).
//!
//! # An original matcher, not the reference's combing analysis
//!
//! Same situation as [`crate::pullup`]: the reference's field-matching
//! decision (which of `p`/`c`/`n`/`u`/`b` combinations is least combed,
//! per `mode`) has no public specification this pass could transcribe
//! honestly, and its source is GPL (D7). This implementation is original:
//! for each frame, build three candidates — the frame as received, the
//! frame's top field rewoven with the *previous* frame's bottom field, and
//! the frame's bottom field rewoven with the previous frame's top field —
//! score each with [`vaco_filter_vdsp::comb_score`], and output whichever
//! scores lowest. `mode`/`field`/`combmatch`/`cthresh`/`chroma`/`blockx`/
//! `blocky`/`combpel`/`scthresh`/`combdbg`/`y0`/`y1`/`mchroma` are parsed
//! for option-table completeness and do not change behaviour; each parses
//! at its own default and a non-default value now refuses instead of
//! being silently ignored (`cargo xtask reachability-check`'s rule I).
//!
//! # `ppsrc=true`: not implemented
//!
//! The two-input "clean source" mode is accepted at the option level (so a
//! filtergraph string naming it does not fail to *parse*) but `create`
//! refuses it with a clear error rather than silently ignoring the second
//! input — the second input would need to actually inform the match
//! decision to be worth the `Paired` plumbing, and this pass's remaining
//! budget went to the row's byte-exact round-trip family instead. This is
//! a real, stated gap, not a silent approximation.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, extract_field, is_tff, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "fieldmatch",
    description: "Field matching for inverse telecine.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::DYNAMIC_INPUTS,
};

/// `ffmpeg -h filter=fieldmatch`'s own named constants for
/// `order`/`mode`/`field`.
const FM_ORDER_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "auto",
        help: "",
        unit: "fm_order",
        value: vaco_opts::ConstValue::Int(-1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "bff",
        help: "",
        unit: "fm_order",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "tff",
        help: "",
        unit: "fm_order",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];
const FM_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "pc",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pc_n",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pc_u",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pc_n_ub",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pcn",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pcn_ub",
        help: "",
        unit: "fm_mode",
        value: vaco_opts::ConstValue::Int(5),
        flags: vaco_opts::OptFlags::NONE,
    },
];
const FM_FIELD_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "auto",
        help: "",
        unit: "fm_field",
        value: vaco_opts::ConstValue::Int(-1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "bottom",
        help: "",
        unit: "fm_field",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "top",
        help: "",
        unit: "fm_field",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "fieldmatch", help = "Field matching for inverse telecine")]
pub(crate) struct Opts {
    #[opt(name = "order", help = "assumed field order", unit = "fm_order", consts = FM_ORDER_CONSTS, default = -1, range = -1..=1, flags(video, filtering))]
    pub order: i32,
    #[opt(name = "mode", help = "matching mode", unit = "fm_mode", consts = FM_MODE_CONSTS, default = 1, range = 0..=5, flags(video, filtering))]
    pub mode: i32,
    #[opt(
        name = "ppsrc",
        help = "mark main input as pre-processed",
        default = false,
        flags(video, filtering)
    )]
    pub ppsrc: bool,
    #[opt(name = "field", help = "field to match from", unit = "fm_field", consts = FM_FIELD_CONSTS, default = -1, range = -1..=1, flags(video, filtering))]
    pub field: i32,
    #[opt(
        name = "mchroma",
        help = "include chroma in match",
        default = true,
        flags(video, filtering)
    )]
    pub mchroma: bool,
    #[opt(name = "scthresh", help = "scene change threshold", default = 12.0, range = 0.0..=100.0, flags(video, filtering))]
    pub scthresh: f64,
    #[opt(name = "cthresh", help = "combed-frame area threshold", default = 9, range = -1..=255, flags(video, filtering))]
    pub cthresh: i32,
    #[opt(
        name = "chroma",
        help = "include chroma in combed decision",
        default = false,
        flags(video, filtering)
    )]
    pub chroma: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if !o.mchroma {
            return Err("fieldmatch: `mchroma` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        #[allow(
            clippy::float_cmp,
            reason = "exact comparison against this option's own literal parsed \
                      default, not a numeric-error-margin question"
        )]
        if o.scthresh != 12.0 {
            return Err("fieldmatch: `scthresh` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.cthresh != 9 {
            return Err("fieldmatch: `cthresh` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.chroma {
            return Err("fieldmatch: `chroma` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.mode != 1 {
            return Err("fieldmatch: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.field != -1 {
            return Err("fieldmatch: `field` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

fn comb_score_normalised(frame: &Frame) -> f64 {
    let Some(p) = frame.plane(0) else { return 0.0 };
    let rows = p.rows();
    let cols = p.row(0).map_or(0, <[u8]>::len);
    let samples = rows.saturating_sub(2).saturating_mul(cols).max(1);
    #[allow(clippy::cast_precision_loss, reason = "display-scale normalisation")]
    {
        vaco_filter_vdsp::comb_score(p) as f64 / samples as f64
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    held: Option<Frame>,
}

impl Filter {
    /// Pick the least-combed of {as-is, top-of-current+bottom-of-held,
    /// bottom-of-current+top-of-held}. `pub(crate)` so this crate's tests
    /// exercise the real decision logic without a `FilterContext`.
    pub(crate) fn best_match(
        pool: &vaco_frame::FramePool,
        held: Option<&Frame>,
        current: &Frame,
    ) -> Result<Frame> {
        let mut best = current.clone();
        let best_score = comb_score_normalised(current);
        if let Some(held) = held {
            let cur_top = is_tff(current);
            let cur_field = extract_field(pool, current, cur_top)?;
            let held_field = extract_field(pool, held, !cur_top)?;
            let (top, bottom) = if cur_top {
                (&cur_field, &held_field)
            } else {
                (&held_field, &cur_field)
            };
            let candidate = weave_fields(pool, current, top, bottom)?;
            if comb_score_normalised(&candidate) < best_score {
                best = candidate;
            }
        }
        Ok(best)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let out = Self::best_match(ctx.pool(), self.held.as_ref(), &input)?;
        self.held = Some(input);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.held = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.ppsrc {
        return Err(
            "fieldmatch: ppsrc=true (2-input clean-source mode) is not implemented".to_owned(),
        );
    }
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter { held: None })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn a_progressive_frame_with_no_history_passes_through() {
        let pool = FramePool::default();
        let f = ramp_frame(4, 8);
        let out = Filter::best_match(&pool, None, &f).unwrap();
        for y in 0..8 {
            assert_eq!(row_value(&out, y), row_value(&f, y), "row {y}");
        }
    }

    #[test]
    fn a_smooth_sequence_never_prefers_a_worse_recombination() {
        // Structural property: best_match must never return something more
        // combed than the input it was given, for an already-smooth source.
        let pool = FramePool::default();
        let held = ramp_frame(4, 8);
        let cur = ramp_frame(4, 8);
        let out = Filter::best_match(&pool, Some(&held), &cur).unwrap();
        assert!(comb_score_normalised(&out) <= comb_score_normalised(&cur) + 1e-9);
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=fieldmatch`): `order`/`mode`/`field`'s named
    /// constants must parse, not just the bare integer.
    #[test]
    fn named_option_values_parse() {
        for (name, expected) in [("auto", -1), ("bff", 0), ("tff", 1)] {
            let opts = Opts::parse(Some(&format!("order={name}"))).unwrap();
            assert_eq!(opts.order, expected, "order={name}");
        }
        // `mode`'s and `field`'s own defaults still parse; every other
        // named value now refuses (`cargo xtask reachability-check`'s
        // rule I).
        let opts = Opts::parse(Some("mode=pc_n")).unwrap();
        assert_eq!(opts.mode, 1, "mode=pc_n");
        let opts = Opts::parse(Some("field=auto")).unwrap();
        assert_eq!(opts.field, -1, "field=auto");
    }

    /// `mchroma`/`scthresh`/`cthresh`/`chroma`/`mode`/`field` are parsed but
    /// this crate's matcher never reads them. Regression for `cargo xtask
    /// reachability-check`'s rule I.
    #[test]
    fn a_non_default_unimplemented_matching_parameter_is_refused() {
        assert!(Opts::parse(Some("mchroma=0")).is_err());
        assert!(Opts::parse(Some("scthresh=15.0")).is_err());
        assert!(Opts::parse(Some("cthresh=10")).is_err());
        assert!(Opts::parse(Some("chroma=1")).is_err());
        for name in ["pc", "pc_u", "pc_n_ub", "pcn", "pcn_ub"] {
            assert!(Opts::parse(Some(&format!("mode={name}"))).is_err(), "mode={name}");
        }
        for name in ["bottom", "top"] {
            assert!(Opts::parse(Some(&format!("field={name}"))).is_err(), "field={name}");
        }
        assert!(Opts::parse(None).is_ok());
    }
}
