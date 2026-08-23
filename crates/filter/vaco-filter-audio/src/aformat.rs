//! `aformat` — constrain a link to one of a list of formats.
//!
//! `aformat` does no conversion itself. Like the reference, it only narrows
//! the negotiated [`FormatSet`] on its output pad to the option-supplied
//! lists; the auto-inserted `aresample` (plan 16 §1.7) does the actual work
//! when the upstream format is not already a member. The filter body is
//! therefore a pure pass-through — the interesting part is entirely in
//! [`create`]'s [`NodeFormats`].
//!
//! `ffmpeg -h filter=aformat` lists three list-valued options, each a
//! `'|'`-separated list with a one-letter alias: `sample_fmts`/`f`,
//! `sample_rates`/`r`, `channel_layouts`/`cl`. All three are implemented.

use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "aformat",
    description: "convert the input audio to one of the specified formats",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(input))
    }
}

/// Split a `'|'`-separated list the way the reference's option splitter does:
/// trim each element, drop empty ones.
fn split_list(raw: &str) -> Vec<String> {
    raw.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let mut out = FormatSet::default();

    if let Some(raw) = req.named("sample_fmts").or_else(|| req.named("f")) {
        let fmts: Vec<SampleFmt> = split_list(&raw)
            .iter()
            .filter_map(|n| SampleFmt::from_name(n).ok())
            .collect();
        if fmts.is_empty() {
            return Err(format!("aformat: no valid sample format in `{raw}`"));
        }
        out.sample_formats = Some(Constraint::OneOf(fmts).normalised());
    }
    if let Some(raw) = req.named("sample_rates").or_else(|| req.named("r")) {
        let rates: Vec<u32> = split_list(&raw)
            .iter()
            .filter_map(|n| n.parse::<u32>().ok())
            .collect();
        if rates.is_empty() {
            return Err(format!("aformat: no valid sample rate in `{raw}`"));
        }
        out.sample_rates = Some(Constraint::OneOf(rates).normalised());
    }
    if let Some(raw) = req.named("channel_layouts").or_else(|| req.named("cl")) {
        let layouts: Vec<ChannelLayout> = split_list(&raw)
            .iter()
            .filter_map(|n| ChannelLayout::from_name(n))
            .collect();
        if layouts.is_empty() {
            return Err(format!("aformat: no valid channel layout in `{raw}`"));
        }
        out.channel_layouts = Some(Constraint::OneOf(layouts).normalised());
    }

    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: vec![out],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Simple::new(Filter)),
    })
}
