//! `segment`/`asegment` — split one input into N outputs at declared
//! boundaries. The structural mirror of `concat`: where `concat` rebases N
//! segments into one continuous stream, this filter cuts one stream into N,
//! and (unlike `concat`) has no reason to rebase anything — each output
//! keeps its slice of the original timeline as-is. Not independently
//! measured against the reference's own timestamp handling; a structural
//! reading of "opposite of concat" (`filters.texi`'s own words for this
//! filter).
//!
//! `ffmpeg -h filter=segment` documents `timestamps` (a `|`-separated list)
//! and `frames` (video) / `samples` (audio); `asegment` is the same shape.
//! The first segment always starts at the beginning of the input and the
//! last always runs to its end, so N boundaries produce N+1 output pads.
//! Each boundary may be prefixed with `+` to make it relative to the
//! previous boundary rather than absolute — `timestamps="+10|+10"` cuts at
//! `10` and `20`, not `10` and `10`.
//!
//! # What decides a cut
//!
//! `frames`/`samples` compares a running frame or sample count against the
//! next boundary; `timestamps` compares the frame's own PTS, converted to
//! seconds via its link time base, against the next boundary in seconds.
//! Setting both is not rejected — `frames`/`samples` wins, since it needs no
//! time-base conversion and is the more literal of the two — but this is a
//! structural choice, not measured (the reference's own precedence between
//! the two was not probed).
//!
//! # Allocation
//!
//! The boundary list is parsed by splitting the option string on `|` and
//! pushing one small `f64`/`i64` per token — proportional to the string an
//! operator actually typed, not to a numeric value multiplied into a much
//! larger one the way `cellauto`'s `size=WxH` was. The output pad count
//! (`boundaries + 1`) is still capped through `vaco_filter_graph::registry
//! ::pads::of`, the same limit `concat`/`split` already enforce, so an
//! absurdly long boundary list is rejected at construction rather than
//! producing an absurd pad count.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

/// One parsed boundary: either a plain frame/sample index or a time in
/// seconds, already resolved from any `+`-relative chain into an absolute
/// value.
#[derive(Debug, Clone, Copy)]
enum Boundary {
    Index(i64),
    Seconds(f64),
}

fn parse_boundaries_indices(spec: &str) -> std::result::Result<Vec<Boundary>, String> {
    let mut out = Vec::new();
    let mut running = 0_i64;
    for tok in spec.split('|') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let relative = tok.starts_with('+');
        let digits = tok.trim_start_matches('+');
        let value: i64 = digits
            .parse()
            .map_err(|_| format!("segment: bad boundary `{tok}`"))?;
        running = if relative { running.saturating_add(value) } else { value };
        out.push(Boundary::Index(running));
    }
    Ok(out)
}

fn parse_boundaries_seconds(spec: &str) -> std::result::Result<Vec<Boundary>, String> {
    let mut out = Vec::new();
    let mut running = 0.0_f64;
    for tok in spec.split('|') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let relative = tok.starts_with('+');
        let d = vaco_core::parse::duration(tok).ok_or_else(|| format!("segment: bad timestamp `{tok}`"))?;
        #[allow(clippy::cast_precision_loss, reason = "display-scale duration conversion")]
        let secs = d.0 as f64 / 1_000_000.0;
        running = if relative { running + secs } else { secs };
        out.push(Boundary::Seconds(running));
    }
    Ok(out)
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "segment", help = "split input into multiple outputs")]
pub(crate) struct Opts {
    #[opt(name = "timestamps", help = "timestamps to split at", default = None, flags(filtering))]
    pub timestamps: Option<String>,
    #[opt(name = "frames", alias = "samples", help = "frame/sample counts to split at", default = None, flags(filtering))]
    pub frames: Option<String>,
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
    boundaries: Vec<Boundary>,
    current: usize,
    n_units: i64,
}

impl Filter {
    /// Whether `frame`'s position has reached or passed the next boundary,
    /// i.e. it belongs to the *following* segment.
    fn past_next_boundary(&self, frame: &vaco_frame::Frame) -> bool {
        let Some(&next) = self.boundaries.get(self.current) else {
            return false;
        };
        match next {
            Boundary::Index(i) => self.n_units >= i,
            Boundary::Seconds(s) => frame.pts.to_seconds(frame.time_base).unwrap_or(0.0) >= s,
        }
    }
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        let outputs = self.boundaries.len() + 1;
        if !ctx.output_has_room(self.current) {
            return Ok(if ctx.output_closed(self.current) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        let Some(frame) = ctx.take_input(0) else {
            if ctx.input_at_eof(0) {
                for p in self.current..outputs {
                    ctx.close_output(p);
                }
                return Ok(Activity::Eof);
            }
            ctx.forward_wanted();
            return Ok(Activity::NeedInput);
        };

        while self.past_next_boundary(&frame) && self.current + 1 < outputs {
            ctx.close_output(self.current);
            self.current += 1;
        }

        let units = match &frame.data {
            FrameData::Audio { samples, .. } => i64::from(*samples),
            FrameData::Video { .. } | FrameData::Subtitle { .. } => 1,
        };
        self.n_units = self.n_units.saturating_add(units);
        ctx.push_output(self.current, frame)?;
        Ok(Activity::Progressed)
    }

    fn flush(&mut self) {
        self.current = 0;
        self.n_units = 0;
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let boundaries = match (&opts.frames, &opts.timestamps) {
        (Some(spec), _) => parse_boundaries_indices(spec)?,
        (None, Some(spec)) => parse_boundaries_seconds(spec)?,
        (None, None) => return Err("segment: one of `timestamps`/`frames` is required".to_owned()),
    };
    let outputs = boundaries
        .len()
        .checked_add(1)
        .ok_or_else(|| "segment: too many boundaries".to_owned())?;
    let output_pads =
        pads::of(media, outputs).ok_or_else(|| "segment: too many outputs".to_owned())?;

    Ok(Instance {
        desc: FilterDesc {
            outputs: output_pads,
            ..desc
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: vec![FormatSet::default(); outputs],
            ties: Tie::all_pads(1, outputs, media),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Filter {
            boundaries,
            current: 0,
            n_units: 0,
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
        name: "segment",
        description: "Split single input stream into multiple video streams",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
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
        name: "asegment",
        description: "Split single input stream into multiple audio streams",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
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

    /// Sends `n` frames (pts `0..n`) through `segment=frames=<spec>` and
    /// returns how many frames landed on each output pad.
    fn run(spec: &str, n: i64, outputs: usize) -> Vec<usize> {
        let args = format!("frames={spec}");
        let req = Instantiate {
            name: "segment",
            instance: "segment",
            args: Some(&args),
            arguments: &[],
        };
        let instance = video::create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sinks: Vec<_> = (0..outputs)
            .map(|i| {
                graph.add_sink(
                    "out",
                    MediaType::Video,
                    vaco_filter_core::mock::any_video_sink(&format!("out{i}")),
                )
            })
            .collect();
        graph.connect(src, 0, node, 0).unwrap();
        for (i, &sink) in sinks.iter().enumerate() {
            graph.connect(node, u32::try_from(i).unwrap(), sink, 0).unwrap();
        }
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();

        let mut counts = vec![0usize; outputs];
        let drain = |graph: &mut Graph, counts: &mut Vec<usize>| loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break true,
                GraphStatus::HasOutput(ready) => {
                    for node_id in ready {
                        if let Some(i) = sinks.iter().position(|&s| s == node_id) {
                            while graph.recv(sinks[i]).is_ok() {
                                counts[i] += 1;
                            }
                        }
                    }
                }
                GraphStatus::NeedInput(_) => break false,
                other => panic!("unexpected graph status: {other:?}"),
            }
        };

        // Interleave sends with draining: the link queue has a finite depth,
        // so sending everything up front before the first `run()` overflows
        // it on any input longer than that depth.
        for i in 0..n {
            graph.send(src, gray_frame(4, 4, i, 0)).unwrap();
            drain(&mut graph, &mut counts);
        }
        graph.close_source(src, vaco_core::Timestamp::new(n)).unwrap();
        while !drain(&mut graph, &mut counts) {}
        counts
    }

    #[test]
    fn one_boundary_splits_into_two_segments() {
        assert_eq!(run("3", 10, 2), vec![3, 7]);
    }

    #[test]
    fn two_boundaries_split_into_three_segments() {
        assert_eq!(run("3|7", 10, 3), vec![3, 4, 3]);
    }

    #[test]
    fn relative_boundaries_accumulate() {
        // "+3|+3" cuts at 3 and 6, matching "3|6" exactly.
        assert_eq!(run("+3|+3", 10, 3), run("3|6", 10, 3));
    }

    #[test]
    fn a_boundary_past_the_end_leaves_a_trailing_empty_segment() {
        assert_eq!(run("3|20", 10, 3), vec![3, 7, 0]);
    }
}
