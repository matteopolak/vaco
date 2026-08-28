//! `streamselect`/`astreamselect` — remap N inputs onto M outputs.
//!
//! `ffmpeg -h filter=streamselect` documents `inputs` (default `2`) and
//! `map` (a list of input indexes, one per output). The reference's own
//! example (`filters.texi`) creates exactly **one** output
//! (`streamselect=inputs=2:map=0`) and switches it between inputs at
//! runtime with `sendcmd='5.0 streamselect map 1'` — so the output count is
//! however many indexes `map` names, not `inputs` itself; an empty `map`
//! defaults to the identity (one output per input, in declaration order).
//!
//! No `framesync`: `-h filter=streamselect` shows neither `eof_action` nor
//! `shortest`/`repeatlast`/`ts_sync_mode`, so per `AGENT-CONSTRAINTS.md`
//! this stays off it — each output is simple lockstep passthrough of
//! whichever single input it currently names.
//!
//! # Runtime `map`
//!
//! `filters.texi`'s "Commands" section lists `map` as a supported runtime
//! command with the same syntax as the option, which is exactly the
//! mechanism the worked `sendcmd` example above depends on. Implemented as
//! [`vaco_filter_core::Filter::command`]: the parsed replacement must name
//! exactly as many outputs as this instance already has (pads are fixed at
//! configuration; a command cannot add or remove one), rejected otherwise.
//!
//! # Known gap: `map` entries are not deduplicated
//!
//! If two outputs name the same input index, only the first one to run in
//! a given `activate` call takes that input's next queued frame; the
//! second sees nothing that pass and picks up a *later* frame next time,
//! silently skipping one. The reference's own worked example only ever
//! has one output active per input at a time, which this implementation
//! handles correctly; genuine fan-out through `map` (two outputs reading
//! the same input simultaneously) is this filter's one documented, not
//! silently-hidden, limitation.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

fn parse_map(value: &str, inputs: usize) -> std::result::Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for tok in value.split(|c: char| c == ',' || c.is_whitespace()) {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let idx: usize = tok
            .parse()
            .map_err(|_| format!("streamselect: bad map index `{tok}`"))?;
        if idx >= inputs {
            return Err(format!("streamselect: map index {idx} out of range for {inputs} inputs"));
        }
        out.push(idx);
    }
    if out.is_empty() {
        out = (0..inputs).collect();
    }
    Ok(out)
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "streamselect", help = "select video or audio streams")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "number of inputs", default = 2, range = 2..=i32::MAX, flags(filtering))]
    pub inputs: i32,
    #[opt(name = "map", help = "input indexes to remap to outputs", default = String::new(), flags(filtering))]
    pub map: String,
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
    /// `map[output_pad] == input_pad`.
    map: Vec<usize>,
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        let outputs = self.map.len();
        if (0..outputs).all(|o| ctx.output_closed(o)) {
            return Ok(Activity::Eof);
        }
        let mut progressed = false;
        let mut blocked = false;
        for (o, &input_pad) in self.map.iter().enumerate() {
            if ctx.output_closed(o) {
                continue;
            }
            if !ctx.output_has_room(o) {
                blocked = true;
                continue;
            }
            if let Some(frame) = ctx.take_input(input_pad) {
                ctx.push_output(o, frame)?;
                progressed = true;
            } else if ctx.input_at_eof(input_pad) {
                ctx.close_output(o);
                progressed = true;
            } else {
                ctx.request_input(input_pad);
            }
        }
        if (0..outputs).all(|o| ctx.output_closed(o)) {
            return Ok(Activity::Eof);
        }
        if progressed {
            return Ok(Activity::Progressed);
        }
        if blocked {
            return Ok(Activity::Blocked);
        }
        Ok(Activity::NeedInput)
    }

    fn flush(&mut self) {}

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        if name != "map" {
            return Err(vaco_core::Error::Unsupported("streamselect: unknown command"));
        }
        let replacement = parse_map(value, self.inputs).map_err(|detail| vaco_core::Error::Option {
            name: "map".to_owned(),
            detail,
        })?;
        if replacement.len() != self.map.len() {
            return Err(vaco_core::Error::Option {
                name: "map".to_owned(),
                detail: "cannot change the output count at runtime".to_owned(),
            });
        }
        self.map = replacement;
        Ok(())
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let inputs = usize::try_from(opts.inputs.max(1)).unwrap_or(1);
    // `inputs` is attacker-controlled option text (`range` only bounds the
    // parsed integer, not what gets allocated from it) and `parse_map`'s
    // empty-`map` fallback builds a `0..inputs`-sized `Vec` — so the pad-count
    // ceiling must be checked *before* that allocation exists, not after.
    // Found by fuzzing: `streamselect=inputs=999999999` with `map` unset
    // requested an 8 GB `Vec<usize>` before this reordering.
    let input_pads =
        pads::of(media, inputs).ok_or_else(|| "streamselect: too many inputs".to_owned())?;
    let map = parse_map(&opts.map, inputs)?;
    let outputs = map.len();
    let output_pads =
        pads::of(media, outputs).ok_or_else(|| "streamselect: too many outputs".to_owned())?;
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            outputs: output_pads,
            ..desc
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); inputs],
            outputs: vec![FormatSet::default(); outputs],
            ties: Tie::all_pads(inputs, outputs, media),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Filter { inputs, map }),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "streamselect",
        description: "Select video streams",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::DYNAMIC_INPUTS.union(FilterFlags::DYNAMIC_OUTPUTS),
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
        name: "astreamselect",
        description: "Select audio streams",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::DYNAMIC_INPUTS.union(FilterFlags::DYNAMIC_OUTPUTS),
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

    /// `inputs=2:map=1` routes the single output entirely from input 1.
    #[test]
    fn map_selects_the_named_input() {
        let req = Instantiate {
            name: "streamselect",
            instance: "streamselect",
            args: Some("inputs=2:map=1"),
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
        for i in 0..3i64 {
            graph.send(a, gray_frame(1, 1, i, 1)).unwrap();
        }
        graph.close_source(a, vaco_core::Timestamp::new(3)).unwrap();
        for i in 0..3i64 {
            graph.send(b, gray_frame(1, 1, i, 2)).unwrap();
        }
        graph.close_source(b, vaco_core::Timestamp::new(3)).unwrap();

        let mut luma = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    while let Ok(f) = graph.recv(sink) {
                        luma.push(f.plane(0).and_then(|p| p.row(0)).and_then(|r| r.first()).copied());
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        assert_eq!(luma, vec![Some(2), Some(2), Some(2)]);
    }

    #[test]
    fn empty_map_defaults_to_identity() {
        assert_eq!(parse_map("", 3).unwrap(), vec![0, 1, 2]);
    }

    /// Fuzz-found: `streamselect=inputs=999999999` with `map` unset used to
    /// request an 8 GB `Vec<usize>` from `parse_map`'s identity fallback
    /// before the pad-count ceiling was ever checked. `build` must reject
    /// this at construction, not allocate its way into an OOM.
    #[test]
    fn a_huge_input_count_is_rejected_before_any_allocation_sized_by_it() {
        let req = Instantiate {
            name: "streamselect",
            instance: "streamselect",
            args: Some("inputs=999999999"),
            arguments: &[],
        };
        assert!(video::create(&req).is_err());
    }

    #[test]
    fn command_rejects_a_different_output_count() {
        let mut f = Filter {
            inputs: 2,
            map: vec![0],
        };
        assert!(f.command("map", "0,1").is_err());
        assert_eq!(f.map, vec![0]);
    }

    #[test]
    fn command_updates_the_map_in_place() {
        let mut f = Filter {
            inputs: 2,
            map: vec![0],
        };
        f.command("map", "1").unwrap();
        assert_eq!(f.map, vec![1]);
    }
}
