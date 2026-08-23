//! `join` — join multiple audio streams into one multi-channel stream.
//!
//! `ffmpeg -h filter=join` documents `inputs` (default 2), `channel_layout`
//! (default `stereo`) and `map` (`input_stream.input_channel-output_channel`,
//! comma-separated). Implemented: the default sequential mapping (input
//! channels concatenated across streams in order, truncated or
//! zero-extended to the output layout's channel count) plus explicit `map`
//! overrides on top of it.
//!
//! Termination follows the same rule as [`crate::amerge`]: the joined stream
//! ends when the first input drains, which is a structural choice rather
//! than a measured one — see that module's docs for why.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad,
};

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

pub const DESC: FilterDesc = FilterDesc {
    name: "join",
    description: "join multiple audio streams into multi-channel output",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::DYNAMIC_INPUTS,
};

/// `(input_stream, input_channel) -> output_channel`.
struct MapEntry {
    stream: usize,
    in_channel: usize,
    out_channel: usize,
}

fn parse_map(raw: &str) -> Vec<MapEntry> {
    let mut out = Vec::new();
    for tok in raw.split(',').map(str::trim) {
        let Some((lhs, rhs)) = tok.split_once('-') else {
            continue;
        };
        let Some((s, c)) = lhs.split_once('.') else {
            continue;
        };
        let (Ok(stream), Ok(in_channel), Ok(out_channel)) =
            (s.parse::<usize>(), c.parse::<usize>(), rhs.parse::<usize>())
        else {
            continue;
        };
        out.push(MapEntry {
            stream,
            in_channel,
            out_channel,
        });
    }
    out
}

#[derive(Debug, Default)]
struct InputState {
    buf: SmallVec<[Vec<f64>; 8]>,
    finished: bool,
    channels: usize,
}

impl InputState {
    fn available(&self) -> usize {
        self.buf.first().map_or(0, Vec::len)
    }
}

pub(crate) struct Join {
    n: usize,
    out_channels: usize,
    /// `out_channel -> Some((stream, in_channel))`.
    map: Vec<Option<(usize, usize)>>,
    inputs: Vec<InputState>,
    out_layout: ChannelLayout,
    out_rate: u32,
    out_format: vaco_sampfmt::SampleFmt,
    pending: std::collections::VecDeque<vaco_frame::Frame>,
    done: bool,
}

impl std::fmt::Debug for Join {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Join")
            .field("n", &self.n)
            .finish_non_exhaustive()
    }
}

impl Join {
    fn default_map(n_inputs: &[usize], out_channels: usize) -> Vec<Option<(usize, usize)>> {
        let mut flat = Vec::new();
        for (s, &count) in n_inputs.iter().enumerate() {
            for c in 0..count {
                flat.push((s, c));
            }
        }
        (0..out_channels).map(|i| flat.get(i).copied()).collect()
    }
}

impl FilterTrait for Join {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        for i in 0..self.n {
            if let Some(LinkFormat::Audio {
                format,
                sample_rate,
                layout,
                ..
            }) = ctx.input_link(i)
            {
                if let Some(state) = self.inputs.get_mut(i) {
                    state.channels = layout.channels.max(1) as usize;
                }
                self.out_rate = *sample_rate;
                self.out_format = *format;
            }
        }
        if self.map.is_empty() {
            let counts: Vec<usize> = self.inputs.iter().map(|s| s.channels).collect();
            self.map = Self::default_map(&counts, self.out_channels);
        }
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                format,
                sample_rate,
                layout,
                ..
            } = &mut out
            {
                *format = self.out_format;
                *sample_rate = self.out_rate;
                *layout = self.out_layout.clone();
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        if let Some(frame) = self.pending.pop_front() {
            if !ctx.output_has_room(0) {
                self.pending.push_front(frame);
                return Ok(Activity::Blocked);
            }
            ctx.push_output(0, frame)?;
            return Ok(Activity::Progressed);
        }
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }

        let mut progressed = false;
        for i in 0..self.n {
            if self.inputs.get(i).is_some_and(|s| s.finished) {
                continue;
            }
            if let Some(frame) = ctx.take_input(i) {
                let (_, _, _, _, channels) = crate::sample::decode(&frame)?;
                if let Some(state) = self.inputs.get_mut(i) {
                    if state.buf.is_empty() {
                        state.buf = channels;
                    } else {
                        for (dst, src) in state.buf.iter_mut().zip(channels) {
                            dst.extend(src);
                        }
                    }
                }
                progressed = true;
            } else if ctx.input_at_eof(i) {
                if let Some(state) = self.inputs.get_mut(i) {
                    state.finished = true;
                }
                progressed = true;
            } else {
                ctx.request_input(i);
            }
        }

        let quota = self
            .inputs
            .iter()
            .map(InputState::available)
            .min()
            .unwrap_or(0);
        let any_drained = self.inputs.iter().any(|s| s.finished && s.available() == 0);

        if quota > 0 {
            let mut out_channels: SmallVec<[Vec<f64>; 8]> = (0..self.out_channels)
                .map(|_| vec![0.0f64; quota])
                .collect();
            for (out_idx, src) in self.map.iter().enumerate() {
                let Some((stream, in_ch)) = src else { continue };
                let Some(state) = self.inputs.get(*stream) else {
                    continue;
                };
                let Some(src_ch) = state.buf.get(*in_ch) else {
                    continue;
                };
                if let Some(dst) = out_channels.get_mut(out_idx) {
                    for (k, slot) in dst.iter_mut().enumerate() {
                        *slot = src_ch.get(k).copied().unwrap_or(0.0);
                    }
                }
            }
            for s in &mut self.inputs {
                for ch in &mut s.buf {
                    let take = quota.min(ch.len());
                    ch.drain(..take);
                }
            }
            if let Ok(frame) = crate::sample::encode(
                &vaco_frame::FramePool::default(),
                self.out_format,
                self.out_layout.clone(),
                self.out_rate,
                &out_channels,
            ) {
                if ctx.output_has_room(0) {
                    ctx.push_output(0, frame)?;
                } else {
                    self.pending.push_back(frame);
                }
            }
            progressed = true;
        }

        if any_drained && quota == 0 {
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }

        if progressed {
            return Ok(Activity::Progressed);
        }
        Ok(Activity::NeedInput)
    }

    fn flush(&mut self) {
        for s in &mut self.inputs {
            s.buf.clear();
            s.finished = false;
        }
        self.pending.clear();
        self.done = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let n_str = req.named("inputs");
    let n = n_str
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2)
        .max(1);
    let layout = req
        .named("channel_layout")
        .and_then(|n| ChannelLayout::from_name(&n))
        .unwrap_or(ChannelLayout::STEREO);
    let map = req.named("map").map(|m| parse_map(&m)).unwrap_or_default();
    let out_channels = layout.channels.max(1) as usize;
    let map_by_out: Vec<Option<(usize, usize)>> = if map.is_empty() {
        Vec::new()
    } else {
        let mut v: Vec<Option<(usize, usize)>> = vec![None; out_channels];
        for e in map {
            if let Some(slot) = v.get_mut(e.out_channel) {
                *slot = Some((e.stream, e.in_channel));
            }
        }
        v
    };

    let input_pads = pads::audio(n).ok_or_else(|| "join: too many inputs".to_owned())?;
    let filter = Join {
        n,
        out_channels,
        map: map_by_out,
        inputs: (0..n).map(|_| InputState::default()).collect(),
        out_layout: layout.clone(),
        out_rate: 0,
        out_format: vaco_sampfmt::SampleFmt::F32,
        pending: std::collections::VecDeque::new(),
        done: false,
    };

    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); n],
            outputs: vec![FormatSet {
                channel_layouts: Some(Constraint::Exact(layout)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(filter),
    })
}
