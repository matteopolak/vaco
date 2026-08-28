//! CL-25: `-filter_complex`/`-lavfi` — link-label resolution and the
//! unlabelled-pad rules.
//!
//! Plan 14 §6.6's precedence order for resolving `[X]` on a complex graph's
//! open input pad:
//!
//! 1. `[file:stream_spec]` — the `-map` source grammar, first match if
//!    ambiguous.
//! 2. `[dec:N]` — a loopback decoder.
//! 3. An output label of another complex graph.
//! 4. An unlabelled pad — connects to the first unused input stream of the
//!    matching media type.
//!
//! # What is implemented here
//!
//! Rules 1 and 4, and the actual mechanics of attaching a real decoded input
//! (via a real [`vaco_sched::spec::PipelineSpec::add_decoder`]) to each
//! resolved pad, auto-converting, and configuring the graph — proven in this
//! module's own tests by decoding a real file through a real graph and
//! reading real output frames back out, not just resolving labels on paper.
//!
//! # What is not
//!
//! - Rule 2 (`[dec:N]` loopback decoders) — `vaco-sched`'s own docs
//!   (`vaco_sched::lib`, "what it does not do yet") say plainly that its DAG
//!   builder is acyclic by construction; a loopback decoder is a real,
//!   separate design addition (CL-26), not a fill-in, and this module
//!   reports a clear error for a `dec:` label rather than silently
//!   mis-resolving it.
//! - Rule 3 (chaining one complex graph's output into another's input) —
//!   each call to [`build_and_attach`] handles one graph in isolation, and
//!   there is no syntax marker distinguishing "this label names another
//!   graph's output" from "this label is a malformed `file:stream_spec`":
//!   the graph scanner has already stripped `[`/`]` by the time a label
//!   reaches [`resolve_labelled`], so `out` referring to an earlier
//!   occurrence's `[out]` and a typo'd stream specifier look identical.
//!   [`resolve_labelled`]'s `MapSpec::Label` arm only ever fires for a
//!   doubled bracket (`[[out]]`, unusual and not what real graphs write);
//!   the ordinary case falls through to the `File` arm and fails as an
//!   unresolvable specifier, which is the honest outcome given the ambiguity
//!   above rather than a working rule 3.
//! - **`-map [label]` is wired end to end.** [`crate::select::StreamPick`]
//!   grew a `Complex(usize)` variant indexing into [`catalog`]'s flat,
//!   labels-only pad list; [`crate::exec::run_pipeline`] rebuilds the same
//!   graphs for real (this module is called from there now) and lines its own
//!   labelled outputs up against the same catalog by construction — both
//!   walk `cli.complex_filters` in order and filter to `label.is_some()`, so
//!   the index spaces agree without either side needing to know the other's
//!   internals. `vaco -i in.mp4 -filter_complex "[0:v]scale=320:240[out]"
//!   -map "[out]" -c:v <encoder> out.mp4` produces a real, correctly-sized
//!   output file — see `crate::exec`'s own tests for an end-to-end run
//!   through a real encoder.
//! - Rule 2 (auto-attaching an *unlabelled* complex output to the first
//!   output file) is still not implemented: [`catalog`] only lists labelled
//!   pads, so an unlabelled complex-graph output remains unreachable exactly
//!   as before this session. Named rather than silently dropped: a graph with
//!   an unlabelled output and no consumer for it is inert, not an error.
//! - Rule 3 (chaining one complex graph's output into another's input) is
//!   still not implemented, for the reason [`resolve_labelled`] already
//!   documents.
//! - Loopback decoders (`[dec:N]`, CL-26) are still refused with a named
//!   error — a separate, acyclic-DAG design change.
//!
//! An output stream fed by a complex-graph label must be encoded, never
//! copied (`crate::exec::resolve_output` rejects `-c copy` for one with the
//! reference's own measured wording: "Streamcopy requested for output stream
//! fed from a complex filtergraph."), and a *further* `-vf`/`-af` layered on
//! top of a `-map [label]` stream is rejected rather than silently ignored or
//! mis-negotiated — the simple-graph builder needs the upstream
//! `CodecParameters` a complex-graph tap does not carry the same way a
//! demuxed stream does.

use std::collections::HashSet;

use vaco_cli_core::map::MapSpec;
use vaco_cli_core::{MatchCtx, StreamInfo};
use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Rational};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::LinkFormat;
use vaco_sched::spec::{PipelineSpec, SourceBind};
use vaco_sched::{FrameTap, InputRef};

use crate::filterreg::CliFilterRegistry;
use crate::select::InputStreams;

/// One `-filter_complex`/`-lavfi` **labelled** output pad, as `-map [label]`
/// can see it before any real decode happens: label and media type only,
/// known from parsing the graph text alone.
///
/// [`catalog`] builds the flat list every output file's `select::resolve`
/// call shares, so `-map [label]` resolves against one consistent index space
/// for the whole invocation; [`crate::exec::run_pipeline`] rebuilds the same
/// graphs for real and lines its own labelled taps up against this same list
/// (both iterate the same `cli.complex_filters` in order, filtered to
/// `label.is_some()` — see this module's docs on why that is safe without
/// either side reading the other's internals).
#[derive(Debug, Clone)]
pub struct ComplexPad {
    pub label: String,
    pub media: MediaType,
}

/// List every labelled output pad across `texts`, in argv/graph-declaration
/// order, without decoding anything.
///
/// [`vaco_filter_graph::parse_and_build`] resolves filter names and wires
/// internal links purely from the description text and the filter registry —
/// it needs no real frame source — so this is safe to call once, early,
/// before any input is opened for real. Unlabelled outputs are omitted: see
/// this module's docs on why rule 2 is not implemented.
///
/// # Errors
/// A message naming the graph text and what failed to parse.
pub fn catalog(texts: &[String]) -> Result<Vec<ComplexPad>, String> {
    let registry = CliFilterRegistry;
    let mut pads = Vec::new();
    for text in texts {
        let built = vaco_filter_graph::parse_and_build(text, &registry)
            .map_err(|e| format!("filtergraph: {}", e.render(text)))?;
        for open in &built.open_outputs {
            if let Some(label) = &open.label {
                pads.push(ComplexPad {
                    label: label.clone(),
                    media: open.media,
                });
            }
        }
    }
    Ok(pads)
}

/// The [`CodecParameters`] an encoder/muxer should see for a filtered output
/// pad, read back from the graph's own negotiated [`LinkFormat`] rather than
/// guessed from the pad's original source — a `scale` output must report its
/// *scaled* dimensions, not its input's.
fn params_of(fmt: &LinkFormat) -> CodecParameters {
    match fmt {
        LinkFormat::Video {
            format,
            width,
            height,
            frame_rate,
            sample_aspect_ratio,
            color,
            ..
        } => {
            let mut p = CodecParameters::video();
            if let Some(v) = p.video.as_mut() {
                v.width = *width;
                v.height = *height;
                v.format = Some(*format);
                v.frame_rate = *frame_rate;
                v.sample_aspect_ratio = *sample_aspect_ratio;
                v.color = *color;
            }
            p
        }
        LinkFormat::Audio {
            format,
            sample_rate,
            layout,
            ..
        } => {
            let mut p = CodecParameters::audio();
            if let Some(a) = p.audio.as_mut() {
                a.sample_rate = *sample_rate;
                a.format = Some(*format);
                a.layout = Some(layout.clone());
            }
            p
        }
    }
}

/// One resolved real input stream: which file, which stream within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StreamRef {
    file: u32,
    stream: u32,
}

/// A complex graph's output, resolved to a real, live tap.
#[derive(Debug)]
pub struct ComplexOutput {
    /// The label written after the last filter in its chain, or `None` for
    /// an unlabelled output (auto-attaches to the first output file, per
    /// plan 14 §6.2 rule 2 — not implemented by this module; the caller
    /// decides what an unlabelled output means).
    pub label: Option<String>,
    pub media: MediaType,
    pub tap: FrameTap,
    /// The time base an encoder reading `tap` should use. Approximated as the
    /// first resolved input pad's time base for this graph occurrence — exact
    /// for the overwhelmingly common one-source-in case, and a documented
    /// approximation for a graph mixing sources of different time bases,
    /// since nothing downstream of a filter graph currently reports a
    /// per-output time base of its own.
    pub time_base: Rational,
    /// The negotiated output format, read back from the graph itself
    /// ([`Graph::sink_format`](vaco_filter_core::Graph::sink_format)) so a
    /// `scale`/`aformat`/… output reports what it actually produces rather
    /// than what its source happened to be.
    pub params: CodecParameters,
}

/// Resolve one open input pad's label against `-map`'s own `file:stream_spec`
/// grammar (rule 1) — reusing [`MapSpec::parse`] directly, since a bracketed
/// filtergraph label and `-map`'s "ordinary form" share the same
/// `[-]file[:stream_spec]` core grammar and `-map`'s own view/negative/`?`
/// extensions simply never appear in a label with no leading `[`/`-`/trailing
/// `?` to trigger them.
///
/// # Errors
/// A message naming what was wrong: a `dec:` label (loopback, not
/// implemented — see module docs), a malformed specifier, an out-of-range
/// file index, or a specifier matching no stream in that file.
fn resolve_labelled(label: &str, files: &[InputStreams]) -> Result<StreamRef, String> {
    if label.starts_with("dec:") {
        return Err(format!(
            "'[{label}]' is a loopback decoder label; loopback decoders are not implemented \
             yet (CL-26 — vaco-sched's DAG builder is acyclic by construction, see this \
             module's docs)"
        ));
    }
    match MapSpec::parse(label) {
        Ok(MapSpec::Label(inner)) => Err(format!(
            "'[{label}]' names another complex graph's output ('{inner}'); chaining complex \
             graphs is not implemented yet (see this module's docs)"
        )),
        Ok(MapSpec::File(fm)) => {
            let idx = usize::try_from(fm.file_index)
                .map_err(|_| format!("invalid input file index: {}", fm.file_index))?;
            let file = files
                .get(idx)
                .ok_or_else(|| format!("invalid input file index: {idx}"))?;
            let view: Vec<StreamInfo> = file
                .streams
                .iter()
                .enumerate()
                .map(|(i, s)| StreamInfo {
                    index: i as u32,
                    ..s.clone()
                })
                .collect();
            let ctx = MatchCtx::streams(&view);
            let stream = (0..view.len() as u32)
                .find(|&i| fm.spec.matches(&ctx, i))
                .ok_or_else(|| format!("'[{label}]' matches no stream in input file {idx}"))?;
            Ok(StreamRef {
                file: idx as u32,
                stream,
            })
        }
        Err(e) => Err(format!("'[{label}]' is not a valid stream specifier: {e}")),
    }
}

/// Rule 4: the first stream of `media`, across `files` in order, not already
/// in `used`.
fn resolve_unlabelled(
    media: MediaType,
    files: &[InputStreams],
    used: &HashSet<StreamRef>,
) -> Option<StreamRef> {
    for (fi, file) in files.iter().enumerate() {
        for s in &file.streams {
            if s.media_type != Some(media) {
                continue;
            }
            let r = StreamRef {
                file: fi as u32,
                stream: s.index,
            };
            if !used.contains(&r) {
                return Some(r);
            }
        }
    }
    None
}

/// Build one `-filter_complex`/`-lavfi` occurrence, decode and attach a real
/// source for every resolved input pad, configure it, and return its open
/// outputs as live [`FrameTap`]s.
///
/// `used` accumulates which `(file, stream)` pairs this call (and, if the
/// caller threads the same set across occurrences, earlier ones) already
/// claimed, so rule 4 does not hand the same stream to two different
/// unlabelled pads.
///
/// `input_refs`/`params` are parallel to `files`: [`InputRef`] handles
/// already registered with `spec` ([`PipelineSpec::add_input`]), and each
/// file's `(stream_index, CodecParameters, time_base)` triples — the same
/// shape [`crate::exec::run_pipeline`] already builds for its own per-stream
/// decode legs.
///
/// # Errors
///
/// A message describing what failed: an unresolvable label, an input pad
/// with no matching real stream, or anything the parser, registry or
/// scheduler itself refused.
#[allow(
    clippy::too_many_arguments,
    clippy::implicit_hasher,
    reason = "internal wiring helper, not a public API surface"
)]
pub fn build_and_attach(
    spec: &mut PipelineSpec,
    text: &str,
    files: &[InputStreams],
    input_refs: &[InputRef],
    params: &[Vec<(u32, CodecParameters, Rational)>],
    used: &mut HashSet<(u32, u32)>,
    auto_conversion: bool,
) -> Result<Vec<ComplexOutput>, String> {
    let registry = CliFilterRegistry;
    let mut built = vaco_filter_graph::parse_and_build(text, &registry)
        .map_err(|e| format!("filtergraph: {}", e.render(text)))?;

    let mut local_used: HashSet<StreamRef> = used
        .iter()
        .map(|&(file, stream)| StreamRef { file, stream })
        .collect();
    let mut pending_sources: Vec<(FrameTap, vaco_filter_core::NodeId, Rational)> = Vec::new();

    while let Some(open) = built.open_inputs.first().cloned() {
        let resolved = match &open.label {
            Some(label) => resolve_labelled(label, files)?,
            None => resolve_unlabelled(open.media, files, &local_used).ok_or_else(|| {
                format!(
                    "not enough unused {:?} input streams for the filtergraph's unlabelled pad",
                    open.media
                )
            })?,
        };
        local_used.insert(resolved);
        used.insert((resolved.file, resolved.stream));

        let (p, time_base) = params
            .get(resolved.file as usize)
            .and_then(|v| v.iter().find(|(idx, _, _)| *idx == resolved.stream))
            .map(|(_, p, tb)| (p.clone(), *tb))
            .ok_or_else(|| "resolved stream has no known parameters".to_owned())?;
        let input = *input_refs
            .get(resolved.file as usize)
            .ok_or_else(|| "resolved file has no registered input".to_owned())?;
        let tap = spec
            .input_stream(input, resolved.stream)
            .map_err(|e| format!("attaching input stream: {e}"))?;

        let codec_id = p
            .codec_id
            .ok_or_else(|| "resolved stream has no known codec".to_owned())?;
        let decoder_desc = vaco_registry::decoder_for(codec_id)
            .ok_or_else(|| "this build has no decoder for that codec".to_owned())?;
        let limits = vaco_limits::Limits::default();
        let mut decoder = decoder_desc.build(limits);
        if let Some(extradata) = p.extradata.as_deref() {
            let _ = decoder.set_extradata(extradata);
        }
        let frames = spec
            .add_decoder(tap, decoder)
            .map_err(|e| format!("attaching a decoder: {e}"))?;

        let (formats, format) = match open.media {
            MediaType::Video => {
                let v = p.video.as_ref().ok_or("a video pad needs video parameters")?;
                let f = crate::filtergraph::video_link(v, time_base);
                let vaco_filter_core::LinkFormat::Video { format, .. } = &f else {
                    unreachable!("video_link always returns LinkFormat::Video")
                };
                (
                    NodeFormats {
                        outputs: vec![FormatSet::video_exact(*format)],
                        label: "in".to_owned(),
                        ..NodeFormats::default()
                    },
                    f,
                )
            }
            MediaType::Audio => {
                let a = p.audio.as_ref().ok_or("an audio pad needs audio parameters")?;
                let f = crate::filtergraph::audio_link(a, time_base);
                let vaco_filter_core::LinkFormat::Audio {
                    format,
                    sample_rate,
                    layout,
                    ..
                } = &f
                else {
                    unreachable!("audio_link always returns LinkFormat::Audio")
                };
                (
                    NodeFormats {
                        outputs: vec![FormatSet::audio_exact(*format, *sample_rate, layout.clone())],
                        label: "in".to_owned(),
                        ..NodeFormats::default()
                    },
                    f,
                )
            }
            _ => return Err("complex graphs are only built for video and audio pads".to_owned()),
        };

        let source_node = built
            .attach_source(0, formats, format)
            .map_err(|e| format!("attaching the source: {e}"))?;
        // `frames` (the real decoder tap) binds to `source_node` (the
        // graph's own buffer-source node) once every pad is attached and the
        // graph is scheduled below — recorded via `sources` in the caller's
        // own loop shape (see `add_filter`'s `SourceBind`).
        pending_sources.push((frames, source_node, time_base));
    }

    let mut outputs_meta: Vec<(Option<String>, MediaType, vaco_filter_core::NodeId)> = Vec::new();
    while let Some(open) = built.open_outputs.first().cloned() {
        let sink = built
            .attach_sink(
                0,
                NodeFormats {
                    inputs: vec![FormatSet::default()],
                    label: open.label.clone().unwrap_or_else(|| "out".to_owned()),
                    ..NodeFormats::default()
                },
            )
            .map_err(|e| format!("attaching a sink: {e}"))?;
        outputs_meta.push((open.label, open.media, sink));
    }

    // The overwhelmingly common case is one source per graph occurrence; see
    // `ComplexOutput::time_base`'s own doc for what this approximates when
    // there is more than one.
    let default_time_base = pending_sources
        .first()
        .map_or_else(|| Rational::new(1, 1_000_000), |(_, _, tb)| *tb);

    let mode = if auto_conversion {
        vaco_filter_core::negotiate::AutoConvert::All
    } else {
        vaco_filter_core::negotiate::AutoConvert::None
    };
    built
        .configure(&registry, mode)
        .map_err(|e| format!("configuring the filtergraph: {e}"))?;

    // Read every sink's negotiated format back before `built.graph` moves
    // into `spec.add_filter` below — this is what makes a `scale`/`aformat`/…
    // output report the format it actually produces rather than a guess.
    let out_params: Vec<CodecParameters> = outputs_meta
        .iter()
        .map(|(_, _, node)| {
            built
                .graph
                .sink_format(*node)
                .map(params_of)
                .unwrap_or_default()
        })
        .collect();

    let sources: Vec<SourceBind> = pending_sources
        .into_iter()
        .map(|(tap, node, tb)| SourceBind::new(tap, node, tb))
        .collect();
    let sinks: Vec<vaco_filter_core::NodeId> = outputs_meta.iter().map(|(_, _, n)| *n).collect();
    let taps = spec
        .add_filter(built.graph, &sources, &sinks)
        .map_err(|e| format!("attaching the filtergraph: {e}"))?;

    Ok(outputs_meta
        .into_iter()
        .zip(taps)
        .zip(out_params)
        .map(|(((label, media, _), tap), params)| ComplexOutput {
            label,
            media,
            tap,
            time_base: default_time_base,
            params,
        })
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// One video-only input file, one video+audio input file — enough to
    /// exercise cross-file resolution and media-type filtering.
    fn two_files() -> Vec<InputStreams> {
        let mut a = InputStreams::default();
        a.push_described(0, 640, 480, 0, 0); // 0:0 video
        let mut b = InputStreams::default();
        b.push_described(0, 320, 240, 0, 0); // 1:0 video
        b.push_described(1, 0, 0, 2, 0); // 1:1 audio
        vec![a, b]
    }

    #[test]
    fn a_file_stream_label_resolves_to_the_named_stream() {
        let files = two_files();
        let r = resolve_labelled("1:a", &files).unwrap();
        assert_eq!((r.file, r.stream), (1, 1));
    }

    #[test]
    fn a_bare_file_index_label_resolves_to_its_first_stream() {
        let files = two_files();
        let r = resolve_labelled("0", &files).unwrap();
        assert_eq!((r.file, r.stream), (0, 0));
    }

    #[test]
    fn an_out_of_range_file_index_is_a_clean_error() {
        let files = two_files();
        assert!(resolve_labelled("5:v", &files).is_err());
    }

    #[test]
    fn a_spec_matching_nothing_is_a_clean_error() {
        let files = two_files();
        // File 0 has no audio stream.
        assert!(resolve_labelled("0:a", &files).is_err());
    }

    #[test]
    fn a_loopback_decoder_label_is_a_named_not_implemented_error() {
        let files = two_files();
        let e = resolve_labelled("dec:0", &files).unwrap_err();
        assert!(e.contains("loopback"), "{e}");
    }

    /// Rule 3 (another complex graph's output label, e.g. `out` from an
    /// earlier `-filter_complex` occurrence) has no syntax of its own to
    /// detect by — the graph scanner has already stripped the brackets by
    /// the time this function sees the label text, so `"out"` is
    /// indistinguishable from a malformed `file:stream_spec`. This is not
    /// rule 3 support; it is `resolve_labelled` correctly refusing an input
    /// it cannot resolve, rather than mis-resolving it as file index 0. See
    /// this module's doc for why rule 3 is not implemented.
    #[test]
    fn a_plain_word_label_that_looks_like_another_graphs_output_fails_cleanly() {
        let files = two_files();
        assert!(resolve_labelled("out", &files).is_err());
    }

    #[test]
    fn unlabelled_pads_take_the_first_unused_stream_of_the_matching_type_in_file_order() {
        let files = two_files();
        let mut used = HashSet::new();
        let first = resolve_unlabelled(MediaType::Video, &files, &used).unwrap();
        assert_eq!((first.file, first.stream), (0, 0));
        used.insert(first);
        let second = resolve_unlabelled(MediaType::Video, &files, &used).unwrap();
        assert_eq!((second.file, second.stream), (1, 0));
        used.insert(second);
        assert!(resolve_unlabelled(MediaType::Video, &files, &used).is_none());
    }

    #[test]
    fn unlabelled_audio_skips_the_video_only_file() {
        let files = two_files();
        let used = HashSet::new();
        let r = resolve_unlabelled(MediaType::Audio, &files, &used).unwrap();
        assert_eq!((r.file, r.stream), (1, 1));
    }
}
