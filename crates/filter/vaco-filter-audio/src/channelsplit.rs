//! `channelsplit` — split audio into per-channel streams.
//!
//! `ffmpeg -h filter=channelsplit` documents `channel_layout` (default
//! `stereo`) and `channels` (default `all`). Because the number of output
//! pads has to be fixed at instantiation time — before format negotiation has
//! run, so before the *real* input layout is known — the reference itself
//! derives the pad count from the declared `channel_layout` option and then
//! constrains the input to match it exactly. This does the same: the input
//! pad's [`FormatSet`] is pinned to `channel_layout` via
//! [`Constraint::Exact`], so a mismatched upstream layout is a negotiation
//! failure rather than a silent wrong pad count.

use vaco_chlayout::{Channel, ChannelLayout};
use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

pub const DESC: FilterDesc = FilterDesc {
    name: "channelsplit",
    description: "split audio into per-channel streams",
    inputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    outputs: &[],
    flags: FilterFlags::DYNAMIC_OUTPUTS,
};

/// Which input channel indices `channels` selects, given `layout`.
fn selected_indices(channels: &str, layout: &ChannelLayout) -> Vec<usize> {
    if channels.trim().is_empty() || channels.trim() == "all" {
        return (0..layout.channels as usize).collect();
    }
    let mut out = Vec::new();
    for tok in channels.split(['|', ',']).map(str::trim) {
        if tok.is_empty() {
            continue;
        }
        if let Ok(n) = tok.parse::<usize>() {
            out.push(n);
        } else if let Some(ch) = Channel::from_name(tok)
            && let Some(idx) = layout.index_of(ch)
        {
            out.push(idx as usize);
        }
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    indices: Vec<usize>,
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if (0..self.indices.len()).any(|p| !ctx.output_has_room(p)) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        if let Some(input) = ctx.take_input(0) {
            let (fmt, rate, samples, _, channels) = crate::sample::decode(&input)?;
            for (pad, &idx) in self.indices.iter().enumerate() {
                let data = channels
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; samples as usize]);
                let ch: smallvec::SmallVec<[Vec<f64>; 8]> = smallvec::smallvec![data];
                let mut f = crate::sample::encode(
                    &vaco_frame::FramePool::default(),
                    fmt,
                    ChannelLayout::MONO,
                    rate,
                    &ch,
                )?;
                f.pts = input.pts;
                f.time_base = input.time_base;
                f.duration = input.duration;
                ctx.push_output(pad, f)?;
            }
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let layout = req
        .named("channel_layout")
        .and_then(|n| ChannelLayout::from_name(&n))
        .unwrap_or(ChannelLayout::STEREO);
    let channels = req.named("channels").unwrap_or_else(|| "all".to_owned());
    let indices = selected_indices(&channels, &layout);
    if indices.is_empty() {
        return Err("channelsplit: no channel selected".to_owned());
    }
    let output_pads =
        pads::audio(indices.len()).ok_or_else(|| "channelsplit: too many outputs".to_owned())?;

    Ok(Instance {
        desc: FilterDesc {
            outputs: output_pads,
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet {
                channel_layouts: Some(Constraint::Exact(layout)),
                ..FormatSet::default()
            }],
            outputs: vec![FormatSet::default(); indices.len()],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Filter { indices }),
    })
}
