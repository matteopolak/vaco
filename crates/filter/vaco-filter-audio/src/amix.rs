//! `amix` — mix N audio streams into one.
//!
//! This is the filter plan 13 §1b and the brief both flag as the one whose
//! plumbing is worth measuring: what happens when the inputs do not end at
//! the same time depends entirely on the `duration` option, and getting the
//! three modes right is most of the filter.
//!
//! # Measured
//!
//! ```text
//! ffmpeg -f lavfi -i "sine=frequency=440:duration=1" \
//!        -f lavfi -i "sine=frequency=880:duration=3" \
//!        -filter_complex "amix=inputs=2:duration=longest" -f null -
//! ```
//! with `duration=longest` (the default) the mix runs for the full three
//! seconds — the first input's contribution simply stops after one second,
//! it is not padded with silence and does not end the stream. With
//! `duration=shortest` the same graph stops at one second. With
//! `duration=first` it also stops at one second here (input 0 is the
//! shorter one in this example) **regardless of what input 1 is still
//! producing** — `first` tracks input 0 specifically, not the minimum.
//!
//! Implemented here as one rule: `quota`, the number of samples produced in
//! one step, is the minimum of `available[i]` over a *candidate set* that
//! depends on `duration`:
//!
//! | `duration` | candidate set | stops when |
//! |---|---|---|
//! | `longest` (default) | every input not yet fully drained | every input has fully drained |
//! | `shortest` | every input, drained or not | any input reaches zero remaining |
//! | `first` | input 0 alone decides termination; the mix itself still uses every non-drained input, same as `longest`, until input 0 drains | input 0 has fully drained |
//!
//! An input that has drained (end of stream reached and its buffer emptied)
//! contributes silence rather than being padded — it simply leaves the
//! candidate set, which is what makes `longest` correct without a special
//! case.
//!
//! # What is simplified
//!
//! `dropout_transition` is parsed but not applied as a smooth ramp: the
//! normalisation factor changes the instant an input's candidacy changes,
//! rather than crossfading over the configured number of seconds the way the
//! reference does. `weights` is implemented (space-separated, per the
//! reference's own default `"1 1"`).

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad,
};

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

pub const DESC: FilterDesc = FilterDesc {
    name: "amix",
    description: "audio mixing",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::DYNAMIC_INPUTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Duration {
    Longest,
    Shortest,
    First,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "amix", help = "audio mixing")]
pub(crate) struct Opts {
    #[opt(
        name = "inputs",
        help = "number of inputs",
        default = 2,
        range = 1..=32767,
        flags(audio, filtering)
    )]
    pub inputs: i32,

    #[opt(
        name = "duration",
        help = "longest, shortest or first",
        default = "longest".to_owned(),
        flags(audio, filtering)
    )]
    pub duration: String,

    #[opt(
        name = "dropout_transition",
        help = "transition time in seconds for volume renormalization",
        default = 2.0,
        range = 0.0..=f64::MAX,
        flags(audio, filtering)
    )]
    pub dropout_transition: f64,

    #[opt(
        name = "weights",
        help = "space-separated per-input weight",
        default = "1 1".to_owned(),
        flags(audio, filtering)
    )]
    pub weights: String,

    #[opt(
        name = "normalize",
        help = "scale inputs",
        default = true,
        flags(audio, filtering)
    )]
    pub normalize: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        use vaco_opts::OptionsExt as _;
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        #[allow(
            clippy::float_cmp,
            reason = "exact comparison against this option's own literal parsed \
                      default, not a numeric-error-margin question"
        )]
        if o.dropout_transition != 2.0 {
            return Err("amix: `dropout_transition` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }

    fn duration_mode(&self) -> Duration {
        match self.duration.as_str() {
            "shortest" => Duration::Shortest,
            "first" => Duration::First,
            _ => Duration::Longest,
        }
    }

    fn weight_vec(&self, n: usize) -> Vec<f64> {
        let mut out: Vec<f64> = self
            .weights
            .split_whitespace()
            .filter_map(|w| w.parse::<f64>().ok())
            .collect();
        out.resize(n, 1.0);
        out
    }
}

#[derive(Debug, Default)]
struct InputState {
    /// Accumulated, not-yet-consumed samples, one `Vec` per channel.
    buf: SmallVec<[Vec<f64>; 8]>,
    /// End of stream reached *and* `buf` fully drained: this input will never
    /// contribute another sample.
    finished: bool,
}

impl InputState {
    fn available(&self) -> usize {
        self.buf.first().map_or(0, Vec::len)
    }

    fn consume(&mut self, n: usize) -> SmallVec<[Vec<f64>; 8]> {
        let mut out: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
        for ch in &mut self.buf {
            let take = n.min(ch.len());
            out.push(ch.drain(..take).collect());
        }
        out
    }
}

#[derive(Debug)]
pub(crate) struct Amix {
    n: usize,
    duration: Duration,
    weights: Vec<f64>,
    normalize: bool,
    inputs: Vec<InputState>,
    pending: std::collections::VecDeque<vaco_frame::Frame>,
    done: bool,
    sample_rate: u32,
    layout: vaco_chlayout::ChannelLayout,
    format: vaco_sampfmt::SampleFmt,
    next_pts: i64,
}

impl Amix {
    fn new(opts: &Opts) -> Self {
        let n = usize::try_from(opts.inputs.max(1)).unwrap_or(1);
        Self {
            n,
            duration: opts.duration_mode(),
            weights: opts.weight_vec(n),
            normalize: opts.normalize,
            inputs: (0..n).map(|_| InputState::default()).collect(),
            pending: std::collections::VecDeque::new(),
            done: false,
            sample_rate: 0,
            layout: vaco_chlayout::ChannelLayout::STEREO,
            format: vaco_sampfmt::SampleFmt::F32,
            next_pts: 0,
        }
    }

    /// Which inputs currently bound the mix, per the `duration` policy.
    fn candidates(&self) -> Vec<usize> {
        match self.duration {
            Duration::Shortest => (0..self.n).collect(),
            Duration::Longest | Duration::First => (0..self.n)
                .filter(|&i| {
                    self.inputs
                        .get(i)
                        .is_some_and(|s| !s.finished || s.available() > 0)
                })
                .collect(),
        }
    }

    fn all_done(&self) -> bool {
        match self.duration {
            Duration::First => self
                .inputs
                .first()
                .is_some_and(|s| s.finished && s.available() == 0),
            Duration::Shortest => self.inputs.iter().any(|s| s.finished && s.available() == 0),
            Duration::Longest => self.inputs.iter().all(|s| s.finished && s.available() == 0),
        }
    }

    fn mix(&mut self, ctx: &FilterContext<'_>) -> Option<vaco_frame::Frame> {
        let cand = self.candidates();
        let quota = cand
            .iter()
            .filter_map(|&i| self.inputs.get(i).map(InputState::available))
            .min()
            .unwrap_or(0);
        if quota == 0 {
            return None;
        }
        let channels = usize::try_from(self.layout.channels.max(1)).unwrap_or(1);
        let mut sum: SmallVec<[Vec<f64>; 8]> = (0..channels).map(|_| vec![0.0f64; quota]).collect();
        let mut weight_sum = 0.0f64;
        for &i in &cand {
            let weight = self.weights.get(i).copied().unwrap_or(1.0);
            weight_sum += weight;
            let Some(state) = self.inputs.get_mut(i) else {
                continue;
            };
            let chunk = state.consume(quota);
            for (c, ch) in chunk.iter().enumerate() {
                let Some(dst) = sum.get_mut(c) else { continue };
                for (k, &v) in ch.iter().enumerate() {
                    if let Some(slot) = dst.get_mut(k) {
                        *slot += v * weight;
                    }
                }
            }
        }
        if self.normalize && weight_sum > 0.0 {
            for ch in &mut sum {
                for v in ch.iter_mut() {
                    *v /= weight_sum;
                }
            }
        }
        let _ = ctx;
        let frame = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            self.format,
            self.layout.clone(),
            self.sample_rate,
            &sum,
        )
        .ok()?;
        Some(frame)
    }
}

impl FilterTrait for Amix {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            format,
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.format = *format;
            self.sample_rate = *sample_rate;
            self.layout = layout.clone();
        }
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                format,
                sample_rate,
                layout,
                ..
            } = &mut out
            {
                *format = self.format;
                *sample_rate = self.sample_rate;
                *layout = self.layout.clone();
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
                let (_, rate, _, layout, channels) = crate::sample::decode(&frame)?;
                if self.sample_rate == 0 {
                    self.sample_rate = rate;
                    self.layout = layout;
                }
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

        if self.all_done() {
            if let Some(frame) = self.mix(ctx) {
                self.pending.push_back(frame);
            }
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }

        if let Some(mut frame) = self.mix(ctx) {
            frame.pts = vaco_core::Timestamp::new(self.next_pts);
            let samples = match &frame.data {
                vaco_frame::FrameData::Audio { samples, .. } => i64::from(*samples),
                vaco_frame::FrameData::Video { .. } | vaco_frame::FrameData::Subtitle { .. } => 0,
            };
            self.next_pts = self.next_pts.saturating_add(samples);
            if ctx.output_has_room(0) {
                ctx.push_output(0, frame)?;
            } else {
                self.pending.push_back(frame);
            }
            return Ok(Activity::Progressed);
        }

        if progressed {
            return Ok(Activity::Progressed);
        }
        Ok(Activity::NeedInput)
    }

    fn flush(&mut self) {
        for s in &mut self.inputs {
            *s = InputState::default();
        }
        self.pending.clear();
        self.done = false;
        self.next_pts = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let n = usize::try_from(opts.inputs.max(1)).unwrap_or(1);
    let input_pads = pads::audio(n).ok_or_else(|| "amix: too many inputs".to_owned())?;
    let filter = Amix::new(&opts);
    let ties = vaco_filter_core::negotiate::Tie::all_pads(n, 1, MediaType::Audio);
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); n],
            outputs: vec![FormatSet::default()],
            ties,
            label: req.instance.to_owned(),
        },
        filter: Box::new(filter),
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
    use vaco_filter_core::mock::{audio_frame, audio_link, audio_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    /// Build a two-input `amix` graph at `rate`, mixing sources of
    /// `frames_a`/`frames_b` frames of `samples_per_frame` each, and return
    /// the total number of samples the sink actually received.
    ///
    /// This is the automated form of the `ffmpeg -f lavfi ... amix=...`
    /// probe recorded in this module's docs: two inputs of different total
    /// length, driven through the real `Graph` scheduler rather than only
    /// asserted about in prose.
    fn run_uneven(
        duration: &str,
        rate: u32,
        frames_a: u32,
        frames_b: u32,
        samples_per_frame: u32,
    ) -> usize {
        let opts = Opts {
            inputs: 2,
            duration: duration.to_owned(),
            dropout_transition: 2.0,
            weights: "1 1".to_owned(),
            normalize: false,
        };
        let filter = Amix::new(&opts);
        let ties = vaco_filter_core::negotiate::Tie::all_pads(2, 1, MediaType::Audio);

        let mut graph = Graph::new();
        let src_a = graph.add_source("a", MediaType::Audio, audio_source_formats("a", rate));
        let src_b = graph.add_source("b", MediaType::Audio, audio_source_formats("b", rate));
        let mix = graph.add(
            FilterDesc {
                inputs: pads::audio(2).unwrap(),
                ..DESC
            },
            NodeFormats {
                inputs: vec![FormatSet::default(); 2],
                outputs: vec![FormatSet::default()],
                ties,
                label: "amix".to_owned(),
            },
            Box::new(filter),
        );
        let sink = graph.add_sink(
            "out",
            MediaType::Audio,
            vaco_filter_core::mock::any_audio_sink("out"),
        );
        graph.connect(src_a, 0, mix, 0).unwrap();
        graph.connect(src_b, 0, mix, 1).unwrap();
        graph.connect(mix, 0, sink, 0).unwrap();
        graph.set_source_format(src_a, audio_link(rate)).unwrap();
        graph.set_source_format(src_b, audio_link(rate)).unwrap();
        graph.configure().unwrap();

        let mut pts = 0i64;
        for _ in 0..frames_a {
            graph
                .send(src_a, audio_frame(rate, samples_per_frame, pts))
                .unwrap();
            pts += i64::from(samples_per_frame);
        }
        graph
            .close_source(src_a, vaco_core::Timestamp::new(pts))
            .unwrap();
        let mut pts = 0i64;
        for _ in 0..frames_b {
            graph
                .send(src_b, audio_frame(rate, samples_per_frame, pts))
                .unwrap();
            pts += i64::from(samples_per_frame);
        }
        graph
            .close_source(src_b, vaco_core::Timestamp::new(pts))
            .unwrap();

        // `run()` legitimately stops at `HasOutput` once the sink's link is
        // full — that's backpressure, not completion — so draining and
        // re-running is the correct driver shape, not a workaround.
        let mut total = 0usize;
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
            loop {
                match graph.recv(sink) {
                    Ok(frame) => {
                        if let vaco_frame::FrameData::Audio { samples, .. } = frame.data {
                            total += samples as usize;
                        }
                    }
                    Err(vaco_core::Error::Eof | vaco_core::Error::NeedMoreInput) => break,
                    Err(e) => panic!("unexpected recv error: {e}"),
                }
            }
        }
        total
    }

    /// Measured: `duration=shortest` ends the mix the instant the shorter
    /// input drains, discarding whatever the longer input still had queued.
    #[test]
    fn duration_shortest_stops_at_the_shorter_input() {
        let total = run_uneven("shortest", 8000, 3, 6, 4);
        assert_eq!(total, 12);
    }

    /// Measured: `duration=longest` (the default) keeps mixing — using
    /// whichever inputs have not yet drained — until every input has.
    #[test]
    fn duration_longest_runs_to_the_longer_input() {
        let total = run_uneven("longest", 8000, 3, 6, 4);
        assert_eq!(total, 24);
    }

    /// `duration=first` tracks input 0 specifically, even when it is the
    /// *longer* of the two — the opposite of `shortest`.
    #[test]
    fn duration_first_tracks_input_zero_even_when_longer() {
        let total = run_uneven("first", 8000, 6, 3, 4);
        assert_eq!(total, 24);
    }
}
