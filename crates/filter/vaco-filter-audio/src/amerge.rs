//! `amerge` — merge N audio streams into one multi-channel stream.
//!
//! Unlike `amix`, there is no mixing arithmetic: each output sample frame is
//! the concatenation of the corresponding sample from every input, in input
//! order. `ffmpeg -h filter=amerge` documents `inputs` (default 2) and
//! `layout_mode` (`legacy`/`reset`/`normal`); the reference's own
//! documentation does not spell out what any of the three actually compute,
//! so this is a best-effort reproduction: `legacy` requests the reference's
//! default layout for the summed channel count when one exists
//! ([`ChannelLayout::default_for`]) and otherwise falls back to the same
//! concatenation `reset`/`normal` always use — one custom layout built by
//! walking each input's channels in order. That is a structural
//! approximation, not a measured match; see `docs/filter/vaco-filter-audio.md`.
//!
//! Termination: like `amix`'s `duration=shortest`, the merged stream ends
//! the moment any one input's buffered samples are exhausted and it has
//! reached end of stream. Untested against the reference's own choice here.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad,
};

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

pub const DESC: FilterDesc = FilterDesc {
    name: "amerge",
    description: "merge two or more audio streams into a single multi-channel stream",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::DYNAMIC_INPUTS,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "amerge", help = "merge audio streams")]
pub(crate) struct Opts {
    #[opt(
        name = "inputs",
        help = "number of inputs",
        default = 2,
        range = 1..=64,
        flags(audio, filtering)
    )]
    pub inputs: i32,

    #[opt(
        name = "layout_mode",
        help = "legacy, reset or normal",
        default = "legacy".to_owned(),
        flags(audio, filtering)
    )]
    pub layout_mode: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        use vaco_opts::OptionsExt as _;
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
struct InputState {
    buf: SmallVec<[Vec<f64>; 8]>,
    finished: bool,
    rate: u32,
    format: vaco_sampfmt::SampleFmt,
    channels: u32,
    layout: ChannelLayout,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            buf: SmallVec::new(),
            finished: false,
            rate: 0,
            format: vaco_sampfmt::SampleFmt::F32,
            channels: 0,
            layout: ChannelLayout::unspecified(1),
        }
    }
}

impl InputState {
    fn available(&self) -> usize {
        self.buf.first().map_or(0, Vec::len)
    }
}

#[derive(Debug)]
pub(crate) struct Amerge {
    n: usize,
    legacy: bool,
    inputs: Vec<InputState>,
    out_layout: Option<ChannelLayout>,
    out_rate: u32,
    out_format: vaco_sampfmt::SampleFmt,
    pending: std::collections::VecDeque<vaco_frame::Frame>,
    done: bool,
}

impl Amerge {
    fn new(opts: &Opts) -> Self {
        let n = usize::try_from(opts.inputs.max(1)).unwrap_or(1);
        Self {
            n,
            legacy: opts.layout_mode != "reset" && opts.layout_mode != "normal",
            inputs: (0..n).map(|_| InputState::default()).collect(),
            out_layout: None,
            out_rate: 0,
            out_format: vaco_sampfmt::SampleFmt::F32,
            pending: std::collections::VecDeque::new(),
            done: false,
        }
    }

    fn resolve_layout(&self) -> ChannelLayout {
        let total: u32 = self.inputs.iter().map(|s| s.channels).sum();
        if self.legacy
            && let Some(l) = ChannelLayout::default_for(total)
        {
            return l;
        }
        let mut chans = Vec::new();
        for s in &self.inputs {
            chans.extend(s.layout.iter());
        }
        ChannelLayout::custom(chans).unwrap_or_else(|| ChannelLayout::unspecified(total.max(1)))
    }
}

impl FilterTrait for Amerge {
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
                    state.format = *format;
                    state.rate = *sample_rate;
                    state.channels = layout.channels.max(1);
                    state.layout = layout.clone();
                }
                self.out_rate = *sample_rate;
                self.out_format = *format;
            }
        }
        let layout = self.resolve_layout();
        self.out_layout = Some(layout.clone());
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                format,
                sample_rate,
                layout: out_layout,
                ..
            } = &mut out
            {
                *format = self.out_format;
                *sample_rate = self.out_rate;
                *out_layout = layout;
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
            let mut merged: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
            for s in &mut self.inputs {
                for ch in &mut s.buf {
                    merged.push(ch.drain(..quota).collect());
                }
            }
            let layout = self.out_layout.clone().unwrap_or(ChannelLayout::STEREO);
            if let Ok(frame) = crate::sample::encode(
                &vaco_frame::FramePool::default(),
                self.out_format,
                layout,
                self.out_rate,
                &merged,
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
    let opts = Opts::parse(req.args)?;
    let n = usize::try_from(opts.inputs.max(1)).unwrap_or(1);
    let input_pads = pads::audio(n).ok_or_else(|| "amerge: too many inputs".to_owned())?;
    let filter = Amerge::new(&opts);
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); n],
            outputs: vec![FormatSet::default()],
            ties: {
                // Each input's own three properties must agree internally
                // (sample_fmt/rate) with the *output*'s sample_fmt/rate, but
                // channel layout is deliberately left untied: that is exactly
                // what this filter changes.
                let mut ties = Vec::new();
                let mut pads_list: Vec<(vaco_filter_core::link::Direction, u32)> = (0..n)
                    .map(|i| (vaco_filter_core::link::Direction::Input, i as u32))
                    .collect();
                pads_list.push((vaco_filter_core::link::Direction::Output, 0));
                ties.push(Tie {
                    property: vaco_filter_core::negotiate::Property::SampleFormat,
                    pads: pads_list.clone(),
                });
                ties.push(Tie {
                    property: vaco_filter_core::negotiate::Property::SampleRate,
                    pads: pads_list,
                });
                ties
            },
            label: req.instance.to_owned(),
        },
        filter: Box::new(filter),
    })
}
