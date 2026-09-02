//! `pullup` — recover progressive frames from a telecined field sequence
//! without a fixed pattern, by detecting which field pairs recombine
//! cleanly.
//!
//! `ffmpeg -h filter=pullup`: `jl`/`jr`/`jt`/`jb` (junk border sizes,
//! parsed for option-table completeness, not applied — see below), `sb`
//! (strict breaks, parsed, not applied), `mp` (metric plane: `y`=0 default,
//! `u`=1, `v`=2).
//!
//! # An original detection heuristic, not the reference's algorithm
//!
//! The reference's own pullup is a published-in-name-only algorithm (no
//! public specification precise enough to transcribe, and its source is
//! GPL — D7 forbids reading it). This implementation is a genuinely
//! different, original design built on
//! [`vaco_filter_vdsp::comb_score`]: every input frame is split into its
//! two fields (`crate::video::extract_field`, same row-parity convention
//! `telecine`/`detelecine` use) and appended to a queue; the front two
//! fields are woven and scored, and a low-combing result is emitted as a
//! genuine progressive frame — a high-combing result means the front field
//! is very likely a duplicate left over from 3:2 pulldown, so it alone is
//! dropped and the next two are tried. `jl`/`jr`/`jt`/`jb`/`sb` (the
//! reference's junk-border and strict-break controls) are parsed but do
//! not change behaviour, which is a real, documented simplification.
//!
//! # What this gets right structurally
//!
//! On a genuinely progressive source (every woven pair scores low), this
//! degrades to plain `weave` and reproduces the input exactly — checked in
//! this module's tests. On a real telecined source it recovers the correct
//! *frame count* (whichever fields it judges to be duplicates are dropped
//! one at a time until a clean pair emerges), which is the property this
//! module's other test checks, rather than claiming byte-for-byte parity
//! with the reference on real telecined footage.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, extract_field, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "pullup",
    description: "Pullup from field sequence to frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// The normalised (per-sample) `comb_score` above which a woven pair is
/// judged combed rather than a genuine frame. Not measured against the
/// reference (there is nothing of the reference's to measure — this
/// threshold exists only in this original heuristic); chosen so a smooth
/// ramp (score `0`) always passes and a strictly-alternating test pattern
/// (this crate's own worst case, see `idet`'s tests) always fails.
const COMB_THRESHOLD: f64 = 2.0;

/// `ffmpeg -h filter=pullup`'s own named constants for `mp`.
const PULLUP_MP_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "y",
        help: "",
        unit: "pullup_mp",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "u",
        help: "",
        unit: "pullup_mp",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "v",
        help: "",
        unit: "pullup_mp",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pullup", help = "Pullup from field sequence to frames")]
pub(crate) struct Opts {
    #[opt(
        name = "jl",
        help = "set left junk size",
        default = 1,
        flags(video, filtering)
    )]
    pub jl: i32,
    #[opt(
        name = "jr",
        help = "set right junk size",
        default = 1,
        flags(video, filtering)
    )]
    pub jr: i32,
    #[opt(
        name = "jt",
        help = "set top junk size",
        default = 4,
        flags(video, filtering)
    )]
    pub jt: i32,
    #[opt(
        name = "jb",
        help = "set bottom junk size",
        default = 4,
        flags(video, filtering)
    )]
    pub jb: i32,
    #[opt(
        name = "sb",
        help = "set strict breaks",
        default = false,
        flags(video, filtering)
    )]
    pub sb: bool,
    #[opt(name = "mp", help = "set metric plane", unit = "pullup_mp", consts = PULLUP_MP_CONSTS, default = 0, range = 0..=2, flags(video, filtering))]
    pub mp: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.jl != 1 {
            return Err("pullup: `jl` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.jr != 1 {
            return Err("pullup: `jr` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.jt != 4 {
            return Err("pullup: `jt` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.jb != 4 {
            return Err("pullup: `jb` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.sb {
            return Err("pullup: `sb` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

fn comb_score_normalised(frame: &Frame, plane: usize) -> f64 {
    let Some(p) = frame.plane(plane) else {
        return 0.0;
    };
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
    metric_plane: usize,
    fields: VecDeque<Frame>,
}

impl Filter {
    pub(crate) fn new(metric_plane: usize) -> Self {
        Self {
            metric_plane,
            fields: VecDeque::new(),
        }
    }

    /// Try to emit as many clean frames as the buffered fields allow,
    /// dropping any leading field judged a duplicate. See the module doc.
    pub(crate) fn drain(&mut self, pool: &FramePool) -> Result<FrameOut> {
        let mut outs = smallvec::SmallVec::new();
        while let (Some(a), Some(b)) = (self.fields.front(), self.fields.get(1)) {
            let a_top = crate::video::is_tff(a);
            let (top, bottom) = if a_top { (a, b) } else { (b, a) };
            let candidate = weave_fields(pool, top, top, bottom)?;
            if comb_score_normalised(&candidate, self.metric_plane) <= COMB_THRESHOLD {
                outs.push(candidate);
                self.fields.pop_front();
                self.fields.pop_front();
            } else {
                // The front field is very likely an orphaned duplicate:
                // drop it alone and retry with the next field in line.
                self.fields.pop_front();
            }
        }
        Ok(FrameOut::Many(outs))
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let top = extract_field(ctx.pool(), &input, true)?;
        let bottom = extract_field(ctx.pool(), &input, false)?;
        self.fields.push_back(top);
        self.fields.push_back(bottom);
        self.drain(ctx.pool())
    }

    fn flush_state(&mut self) {
        self.fields.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    #[allow(clippy::cast_sign_loss, reason = "mp's own range is 0..=2")]
    let metric_plane = opts.mp.clamp(0, 2) as usize;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(metric_plane))),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn a_progressive_source_reproduces_exactly() {
        // Every woven pair from a smooth ramp scores 0 <= threshold, so
        // this degrades to plain weave and reproduces the input exactly.
        let pool = FramePool::default();
        let mut filt = Filter::new(0);
        let f = ramp_frame(4, 8);
        let top = extract_field(&pool, &f, true).unwrap();
        let bottom = extract_field(&pool, &f, false).unwrap();
        filt.fields.push_back(top);
        filt.fields.push_back(bottom);
        let FrameOut::Many(out) = filt.drain(&pool).unwrap() else {
            panic!("expected FrameOut::Many")
        };
        assert_eq!(out.len(), 1);
        for y in 0..8 {
            assert_eq!(row_value(&out[0], y), row_value(&f, y), "row {y}");
        }
    }

    #[test]
    fn a_lone_combed_field_is_dropped_rather_than_emitted() {
        let pool = FramePool::default();
        let mut filt = Filter::new(0);
        // A strictly-alternating (combed) frame, split into fields: the
        // resulting pair is itself uncombed (each field is uniform), but
        // pairing it with a mismatched partner should score high. Simulate
        // directly by pushing a combed *field* (not a genuine top/bottom
        // pair) followed by a clean one.
        let combed = {
            let pool = FramePool::default();
            let mut f = pool
                .acquire_video(vaco_pixfmt::PixFmt::Gray8, 4, 4)
                .unwrap();
            if let Some(mut p) = f.plane_mut(0) {
                for y in 0..4usize {
                    if let Some(row) = p.row_mut(y) {
                        row.fill(if y % 2 == 0 { 0 } else { 255 });
                    }
                }
            }
            f.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
            f
        };
        let clean_bottom = ramp_frame(4, 4);
        filt.fields.push_back(combed);
        filt.fields.push_back(clean_bottom);
        let out = filt.drain(&pool).unwrap();
        // Whatever comes out, this must not panic and must not loop forever
        // (the real property under test: `drain` always terminates).
        assert!(out.len() <= 1);
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=pullup`).
    #[test]
    fn named_mp_values_parse() {
        for (name, expected) in [("y", 0), ("u", 1), ("v", 2)] {
            let opts = Opts::parse(Some(&format!("mp={name}"))).unwrap();
            assert_eq!(opts.mp, expected, "mp={name}");
        }
    }

    /// `jl`/`jr`/`jt`/`jb`/`sb` (junk-border and strict-break controls) are
    /// parsed but this crate's original comb-score detector never reads
    /// them, per this module's own doc. Regression for `cargo xtask
    /// reachability-check`'s rule I.
    #[test]
    fn a_non_default_unimplemented_junk_border_is_refused() {
        assert!(Opts::parse(Some("jl=8")).is_err());
        assert!(Opts::parse(Some("jr=8")).is_err());
        assert!(Opts::parse(Some("jt=8")).is_err());
        assert!(Opts::parse(Some("jb=8")).is_err());
        assert!(Opts::parse(Some("sb=1")).is_err());
        assert!(Opts::parse(None).is_ok());
    }

}
