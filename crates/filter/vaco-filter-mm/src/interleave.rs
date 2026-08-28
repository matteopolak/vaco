//! `interleave`/`ainterleave` — merge several inputs, oldest frame first.
//!
//! `ffmpeg -h filter=interleave` documents `nb_inputs`/`n` (default `2`)
//! and `duration` (`longest`/`shortest`/`first`, default `longest`). Per
//! `filters.texi`: these filters read one frame ahead on every input and
//! always emit whichever queued frame has the smallest timestamp, so every
//! input must have "well defined, monotonically increasing" timestamps —
//! if one input never produces a frame (its own doc example: a `select`
//! that drops everything), this filter cannot make progress on it either,
//! which is a property of the merge rule, not a bug in this implementation.
//!
//! `duration` decides when the merge itself ends: `longest` (the default)
//! keeps going until every input reaches end of stream; `shortest` stops as
//! soon as *any* input does; `first` stops when input `0` does, regardless
//! of the others. Structural reading of the option's three names, not
//! independently measured against the reference's own frame-for-frame
//! output near the end of a genuinely uneven set of inputs.
//!
//! No `framesync` here: `-h filter=interleave` shows neither `eof_action`
//! nor `shortest`/`repeatlast`/`ts_sync_mode` — `duration` is this filter's
//! own, differently-shaped answer to the same question `framesync` answers
//! elsewhere, so `AGENT-CONSTRAINTS.md`'s "two inputs does not mean
//! framesync" applies here even at N inputs.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "interleave_duration", base = "int")]
pub(crate) enum Duration {
    #[opt_const(name = "longest", help = "duration of the longest input")]
    #[default]
    Longest,
    #[opt_const(name = "shortest", help = "duration of the shortest input")]
    Shortest,
    #[opt_const(name = "first", help = "duration of the first input")]
    First,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "interleave", help = "temporally interleave frames from several inputs")]
pub(crate) struct Opts {
    #[opt(name = "nb_inputs", alias = "n", help = "number of inputs", default = 2, range = 1..=i32::MAX, flags(filtering))]
    pub nb_inputs: i32,
    #[opt(
        name = "duration",
        help = "how to determine the end-of-stream",
        unit = "interleave_duration",
        default = Duration::Longest,
        default_repr = "longest",
        flags(filtering)
    )]
    pub duration: Duration,
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

#[derive(Debug)]
pub(crate) struct Filter {
    inputs: usize,
    duration: Duration,
    /// Sticky once an input hits end of stream, so a later `activate` does
    /// not keep asking a link that will never answer again.
    at_eof: Vec<bool>,
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if !ctx.output_has_room(0) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }

        for (p, eof) in self.at_eof.iter_mut().enumerate() {
            if !*eof && ctx.input_at_eof(p) {
                *eof = true;
            }
        }

        let any_eof = self.at_eof.iter().any(|&e| e);
        let all_eof = self.at_eof.iter().all(|&e| e);
        let first_eof = self.at_eof.first().copied().unwrap_or(false);
        let done = all_eof
            || (self.duration == Duration::Shortest && any_eof)
            || (self.duration == Duration::First && first_eof);
        if done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }

        // Every still-relevant input needs a peeked frame before a fair
        // comparison can be made; an input this merge has already decided
        // to ignore (past its own eof) is skipped.
        let mut best: Option<(usize, f64)> = None;
        for p in 0..self.inputs {
            if self.at_eof.get(p).copied().unwrap_or(true) {
                continue;
            }
            let Some(frame) = ctx.peek_input(p) else {
                ctx.request_input(p);
                return Ok(Activity::NeedInput);
            };
            let tb = ctx.input_link(p).map_or(vaco_core::Rational::UNDEFINED, |l| match l {
                vaco_filter_core::LinkFormat::Video { time_base, .. }
                | vaco_filter_core::LinkFormat::Audio { time_base, .. } => *time_base,
            });
            let t = frame.pts.to_seconds(tb).unwrap_or(f64::INFINITY);
            if best.is_none_or(|(_, bt)| t < bt) {
                best = Some((p, t));
            }
        }

        let Some((pad, _)) = best else {
            // Every input already past its own eof but the merge as a
            // whole is not done (`longest` waiting on a later input): loop
            // back around next time.
            return Ok(Activity::NeedInput);
        };
        let Some(frame) = ctx.take_input(pad) else {
            return Ok(Activity::NeedInput);
        };
        ctx.push_output(0, frame)?;
        Ok(Activity::Progressed)
    }

    fn flush(&mut self) {
        self.at_eof.iter_mut().for_each(|e| *e = false);
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let inputs = usize::try_from(opts.nb_inputs.max(1)).unwrap_or(1);
    let input_pads =
        pads::of(media, inputs).ok_or_else(|| "interleave: too many inputs".to_owned())?;
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..desc
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); inputs],
            outputs: vec![FormatSet::default()],
            ties: Tie::all_pads(inputs, 1, media),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Filter {
            inputs,
            duration: opts.duration,
            at_eof: vec![false; inputs],
        }),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "interleave",
        description: "Temporally interleave video inputs",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::DYNAMIC_INPUTS,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Video, DESC, req)
    }
}

pub mod audio {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const AUDIO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "ainterleave",
        description: "Temporally interleave audio inputs",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::DYNAMIC_INPUTS,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
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
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    /// Two inputs, even and odd pts respectively, merged by `interleave`.
    /// The output must be the fully sorted union, oldest first.
    #[test]
    fn merges_two_inputs_in_timestamp_order() {
        let req = Instantiate {
            name: "interleave",
            instance: "interleave",
            args: Some("n=2"),
            arguments: &[],
        };
        let instance = video::create(&req).unwrap();
        let mut graph = Graph::new();
        let a = graph.add_source("a", MediaType::Video, video_source_formats("a", vaco_pixfmt::PixFmt::Gray8));
        let b = graph.add_source("b", MediaType::Video, video_source_formats("b", vaco_pixfmt::PixFmt::Gray8));
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink("out", MediaType::Video, vaco_filter_core::mock::any_video_sink("out"));
        graph.connect(a, 0, node, 0).unwrap();
        graph.connect(b, 0, node, 1).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(a, gray_link(1, 1, tb)).unwrap();
        graph.set_source_format(b, gray_link(1, 1, tb)).unwrap();
        graph.configure().unwrap();

        for i in [0, 2, 4] {
            graph.send(a, gray_frame(1, 1, i, 0)).unwrap();
        }
        graph.close_source(a, vaco_core::Timestamp::new(6)).unwrap();
        for i in [1, 3, 5] {
            graph.send(b, gray_frame(1, 1, i, 0)).unwrap();
        }
        graph.close_source(b, vaco_core::Timestamp::new(6)).unwrap();

        let mut pts = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    while let Ok(f) = graph.recv(sink) {
                        pts.push(f.pts.ticks().unwrap_or(-1));
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        assert_eq!(pts, vec![0, 1, 2, 3, 4, 5]);
    }
}
