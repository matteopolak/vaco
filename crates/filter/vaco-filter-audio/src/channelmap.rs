//! `channelmap` — remap audio channels.
//!
//! `ffmpeg -h filter=channelmap` documents `map` ("a comma-separated list of
//! input channel numbers in output order") and `channel_layout` (the output
//! layout). The reference's own filter documentation additionally allows
//! channel *names* and an explicit `in-out` form, and separates entries with
//! either `|` or `,` depending on which doc page you read — since both are
//! observed in the wild and accepting one when the reference wants the other
//! only breaks a script, this accepts both (D17's converse case: being a
//! permissive superset breaks nothing that already worked).
//!
//! Grammar implemented for one `map` entry: `IN` or `IN-OUT`, where `IN`/`OUT`
//! are each a bare 0-based input/output channel index. A bare `IN` fills
//! output channels sequentially in the order its entry appears.
//!
//! **Not implemented**: resolving `IN`/`OUT` by channel *name* (`FL-FR`).
//! The reference accepts names there; this accepts indices only. Structurally
//! present, not measured against the reference for the name form.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "channelmap",
    description: "remap audio channels",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// A generous ceiling on channel indices this filter will act on.
///
/// Not a real channel-count limit — [`vaco_filter_graph::registry::pads::MAX`]
/// is the actual cap the graph layer enforces — but a bound `parse_map` needs
/// *before* that layer ever sees the result, because an out-of-range index
/// here feeds straight into `Vec::resize`. A fuzz run found exactly this:
/// `channelmap=map=0-88888888888888888` parsed the right-hand side as a huge
/// but valid `usize` and asked for a multi-exabyte allocation. Any index past
/// this bound is treated as unresolvable, the same as text that fails to
/// parse as a number at all.
const MAX_CHANNEL_INDEX: usize = 4096;

fn resolve_channel(token: &str) -> Option<usize> {
    token
        .parse::<usize>()
        .ok()
        .filter(|&n| n <= MAX_CHANNEL_INDEX)
}

/// Parse `map` into `(output index -> Some(input index))`, `None` meaning
/// silence at that output position.
fn parse_map(raw: &str) -> Vec<Option<usize>> {
    let tokens: Vec<&str> = raw
        .split(['|', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut out: Vec<Option<usize>> = Vec::new();
    for (seq, token) in tokens.iter().enumerate() {
        if let Some((lhs, rhs)) = token.split_once('-') {
            let Some(input_idx) = resolve_channel(lhs) else {
                continue;
            };
            let Some(out_idx) = resolve_channel(rhs) else {
                continue;
            };
            if out.len() <= out_idx {
                out.resize(out_idx + 1, None);
            }
            if let Some(slot) = out.get_mut(out_idx) {
                *slot = Some(input_idx);
            }
        } else if let Some(input_idx) = resolve_channel(token) {
            if out.len() <= seq {
                out.resize(seq + 1, None);
            }
            if let Some(slot) = out.get_mut(seq) {
                *slot = Some(input_idx);
            }
        }
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    map: Vec<Option<usize>>,
    out_layout: Option<ChannelLayout>,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let out_len = if self.map.is_empty() {
            match ctx.input_link(0) {
                Some(LinkFormat::Audio { layout, .. }) => layout.channels.max(1) as usize,
                _ => 1,
            }
        } else {
            self.map.len()
        };
        if self.map.is_empty() {
            self.map = (0..out_len).map(Some).collect();
        }
        let layout = self.out_layout.clone().unwrap_or_else(|| {
            ChannelLayout::default_for(u32::try_from(out_len).unwrap_or(1))
                .unwrap_or_else(|| ChannelLayout::unspecified(u32::try_from(out_len).unwrap_or(1)))
        });
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio { layout: out_l, .. } = &mut out {
                *out_l = layout;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, samples, _, channels) = crate::sample::decode(&input)?;
        let mut mapped: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
        for slot in &self.map {
            match slot.and_then(|i| channels.get(i)) {
                Some(ch) => mapped.push(ch.clone()),
                None => mapped.push(vec![0.0; samples as usize]),
            }
        }
        let layout = ChannelLayout::default_for(u32::try_from(mapped.len()).unwrap_or(1))
            .unwrap_or_else(|| {
                ChannelLayout::unspecified(u32::try_from(mapped.len()).unwrap_or(1))
            });
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &mapped,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> signature every filter in this crate's registry.rs dispatches through, even though this particular filter never fails today"
)]
pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let map_raw = req.named("map").or_else(|| req.positional(0));
    let map = map_raw.as_deref().map(parse_map).unwrap_or_default();
    let out_layout = req
        .named("channel_layout")
        .and_then(|n| ChannelLayout::from_name(&n));

    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: vec![FormatSet::default()],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Simple::new(Filter { map, out_layout })),
    })
}
