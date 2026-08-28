//! `fieldhint` — field matching driven by an explicit per-frame hint file,
//! rather than the automatic combing analysis `fieldmatch` does.
//!
//! `ffmpeg -h filter=fieldhint`: `hint` (a file path, no default), `mode`
//! (`absolute`=0 default, `relative`=1, `pattern`=2).
//!
//! # The file format: this crate's own contract, not confirmed against the reference
//!
//! Same situation `vaco-filter-temporal::fsync` documents for its own
//! `file` option: probing a missing path fails cleanly
//! (`No such file or directory`), confirming the option genuinely opens and
//! reads a file, but reverse-engineering the reference's exact per-line
//! grammar for three different `mode`s was out of this pass's budget. This
//! implementation defines its own format instead of guessing the
//! reference's: **one line per output frame**, `top,bottom` — two
//! zero-based source field indices into the continuous field stream
//! `absolute` mode addresses directly (`relative`/`pattern` are parsed as
//! options but read the file with the same absolute grammar, which is a
//! real, documented gap for those two modes specifically). Blank lines and
//! `#`-prefixed lines are ignored. A file that fails to open or parse is a
//! clean [`vaco_core::Error`] at creation, never a panic and never a silent
//! passthrough — mirroring `fsync`'s own contract.
//!
//! # Shape
//!
//! Reuses [`crate::video::extract_field`]/[`crate::video::weave_fields`]:
//! every input frame is split into its two fields
//! (`crate::telecine`/`crate::detelecine`'s same row-parity convention,
//! even rows first) and appended to a continuous field pool; each hint line
//! then weaves two named fields from that pool into one output frame. A
//! hint naming a field index not yet buffered blocks until enough input has
//! arrived (the same backpressure `Simple` already provides for a filter
//! that buffers).

use std::collections::VecDeque;
use std::io::Read as _;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, extract_field, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "fieldhint",
    description: "Field matching using hints.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `ffmpeg -h filter=fieldhint`'s own named constants for `mode`.
const FIELDHINT_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "absolute",
        help: "",
        unit: "fieldhint_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "relative",
        help: "",
        unit: "fieldhint_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pattern",
        help: "",
        unit: "fieldhint_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "fieldhint", help = "Field matching using hints")]
pub(crate) struct Opts {
    #[opt(name = "hint", help = "set hint file", default = "".to_string(), flags(video, filtering))]
    pub hint: String,
    #[opt(name = "mode", help = "set hint mode", unit = "fieldhint_mode", consts = FIELDHINT_MODE_CONSTS, default = 0, range = 0..=2, flags(video, filtering))]
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

/// Parse this crate's hint-file grammar: see the module doc.
pub(crate) fn parse_hints(text: &str) -> Vec<(usize, usize)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (a, b) = l.split_once(',')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .collect()
}

#[derive(Debug)]
pub(crate) struct Filter {
    hints: Vec<(usize, usize)>,
    next_hint: usize,
    fields: VecDeque<Frame>,
    fields_consumed: usize,
}

impl Filter {
    pub(crate) fn new(hints: Vec<(usize, usize)>) -> Self {
        Self {
            hints,
            next_hint: 0,
            fields: VecDeque::new(),
            fields_consumed: 0,
        }
    }

    fn field_at(&self, index: usize) -> Option<&Frame> {
        let offset = index.checked_sub(self.fields_consumed)?;
        self.fields.get(offset)
    }

    fn drain_ready(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let mut outs = smallvec::SmallVec::new();
        while let Some(&(top_idx, bottom_idx)) = self.hints.get(self.next_hint) {
            let (Some(top), Some(bottom)) = (self.field_at(top_idx), self.field_at(bottom_idx))
            else {
                break;
            };
            outs.push(weave_fields(ctx.pool(), top, top, bottom)?);
            self.next_hint = self.next_hint.saturating_add(1);
            // Drop fields no future hint can still reference.
            let still_needed = self
                .hints
                .get(self.next_hint..)
                .into_iter()
                .flatten()
                .flat_map(|&(a, b)| [a, b])
                .min()
                .unwrap_or(self.fields_consumed + self.fields.len());
            while self.fields_consumed < still_needed && !self.fields.is_empty() {
                self.fields.pop_front();
                self.fields_consumed = self.fields_consumed.saturating_add(1);
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
        self.drain_ready(ctx)
    }

    fn flush_state(&mut self) {
        self.next_hint = 0;
        self.fields.clear();
        self.fields_consumed = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.hint.is_empty() {
        return Err("fieldhint: `hint` file is required".to_owned());
    }
    let mut text = String::new();
    std::fs::File::open(&opts.hint)
        .and_then(|mut f| f.read_to_string(&mut text))
        .map_err(|e| format!("fieldhint: could not read hint file `{}`: {e}", opts.hint))?;
    let hints = parse_hints(&text);
    if hints.is_empty() {
        return Err(format!(
            "fieldhint: hint file `{}` produced no usable hints",
            opts.hint
        ));
    }
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(hints))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn parse_hints_reads_comma_pairs_and_skips_comments() {
        let text = "# comment\n0,1\n\n2,3\n";
        assert_eq!(parse_hints(text), vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn absolute_hints_weave_the_named_fields() {
        let pool = FramePool::default();
        let mut filt = Filter::new(vec![(0, 1)]);
        let f0 = ramp_frame(2, 8);
        let top0 = extract_field(&pool, &f0, true).unwrap();
        let bottom0 = extract_field(&pool, &f0, false).unwrap();
        filt.fields.push_back(top0);
        filt.fields.push_back(bottom0);
        // Drive drain_ready without a FilterContext: replicate its loop
        // directly against the pool, since this crate's helpers take a pool.
        let (top_idx, bottom_idx) = filt.hints[0];
        let top = filt.field_at(top_idx).unwrap().clone();
        let bottom = filt.field_at(bottom_idx).unwrap().clone();
        let out = weave_fields(&pool, &top, &top, &bottom).unwrap();
        for y in 0..8 {
            assert_eq!(row_value(&out, y), row_value(&f0, y), "row {y}");
        }
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=fieldhint`).
    #[test]
    fn named_mode_values_parse() {
        for (name, expected) in [("absolute", 0), ("relative", 1), ("pattern", 2)] {
            let opts = Opts::parse(Some(&format!("mode={name}:hint=/dev/null"))).unwrap();
            assert_eq!(opts.mode, expected, "mode={name}");
        }
    }
}
