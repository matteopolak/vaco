//! From bound options to a running pipeline.
//!
//! # The shape of a run
//!
//! ```text
//! open every input      (input.rs)      -> demuxers + stream metadata
//! resolve every output  (this module)   -> a muxer, or a diagnosis
//! select streams        (select.rs)     -> which input stream goes where
//! resolve codecs        (this module)   -> `copy`, or a diagnosis
//! build a PipelineSpec  (vaco-sched)    -> map(tap, output, params) per stream
//! drive it              (vaco-sched)    -> Finish
//! ```
//!
//! # There are no muxers, and that is the whole design constraint
//!
//! D5 puts zero muxers in v0.1, so [`muxer_for`] can return exactly one thing:
//! the [`null`](crate::nullmux) sink. Everything else is refused, and the
//! refusal distinguishes three cases so the message names the real reason:
//!
//! | `-f` / extension | message | exit |
//! |---|---|---|
//! | a format this build can **read** (`matroska`, `mp4`, `mpegts`) | "no muxer for 'x': this build reads that format but cannot write it" | 8 |
//! | a name nothing claims (`-f nosuchformat`) | the reference's own "Requested output format 'x' is not known." | 234 |
//! | no `-f` and an unhelpful extension | the reference's own "Unable to choose an output format for 'x'…" | 234 |
//!
//! The second and third are byte-identical to `ffmpeg` 8.1 modulo the pointer
//! it prints in its log prefix. The first has no reference behaviour to match,
//! because the reference is never built without muxers; `AVERROR_MUXER_NOT_FOUND`
//! is the code that names the situation, and it exits 8 like every other
//! four-character tag.
//!
//! # There are no encoders either
//!
//! So an output stream must be `-c copy`. Without one, the run takes the
//! reference's *own* path for a build missing an encoder — "Default encoder for
//! format null (codec none) is probably disabled" — which is a message it
//! already emits for exactly this situation and which is therefore the right
//! one to reproduce rather than invent.

use vaco_cli_core::{MatchCtx, StreamInfo};
use vaco_core::{MediaType, Result};
use vaco_format_core::Stream;
use vaco_sched::{Driver, Finish, PipelineSpec};

use crate::cli::{Cli, OutputSpec};
use crate::exit::{AvError, Diagnostic};
use crate::input::InputFile;
use crate::nullmux::{NullMuxer, OutputTally, Sink};
use crate::select::{self, InputStreams, StreamPick};

/// One resolved output stream, in output order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutStream {
    pub source: StreamPick,
    pub media: Option<MediaType>,
}

/// What one output file resolved to.
#[derive(Debug)]
pub struct ResolvedOutput {
    pub index: u32,
    pub url: String,
    pub format: &'static str,
    pub streams: Vec<OutStream>,
    pub sink: Sink,
    /// Every `-map` on this output matched nothing, so the reference drops the
    /// file and exits 0. See [`crate::select::Selection::dropped`].
    pub dropped: bool,
}

/// Everything a completed run reports.
#[derive(Debug, Default)]
pub struct RunSpec {
    /// Per output, the packets and bytes that reached it.
    pub tallies: Vec<OutputTally>,
    /// `Stream #0:0 -> #0:0 (copy)`, one per output stream, in order.
    pub mapping: Vec<String>,
    /// The per-output summary line the reference prints at the end.
    pub summary: Vec<String>,
}

/// Build the selection view of one opened input.
#[must_use]
pub fn describe(input: &InputFile) -> InputStreams {
    let streams = input.demuxer.streams();
    InputStreams {
        streams: streams.iter().map(stream_info).collect(),
        programs: input
            .demuxer
            .programs()
            .iter()
            .map(|p| vaco_cli_core::ProgramInfo {
                id: p.id,
                streams: p.stream_indices.clone(),
            })
            .collect(),
        channels: streams.iter().map(channels_of).collect(),
    }
}

fn channels_of(s: &Stream) -> u32 {
    s.params
        .audio
        .as_ref()
        .and_then(|a| a.layout.as_ref())
        .map_or(0, |l| l.channels)
}

fn stream_info(s: &Stream) -> StreamInfo {
    let mut tags = vaco_core::Dict::new();
    for (k, v) in &s.metadata {
        tags.set(k, v);
    }
    StreamInfo {
        index: s.index,
        id: s.id.unwrap_or(0),
        media_type: s.params.media_type,
        // D19 records these two `Disposition` types as bit-for-bit aligned —
        // 19 flags, same order — with the shared home still to be chosen. The
        // conversion is a bit copy, and `dispositions_are_aligned` below fails
        // the build if that ever stops being true.
        disposition: vaco_cli_core::Disposition::from_bits(s.disposition.bits()),
        tags,
        codec_known: s.params.codec_id.is_some(),
        width: s.params.video.as_ref().map_or(0, |v| v.width),
        height: s.params.video.as_ref().map_or(0, |v| v.height),
        sample_rate: s.params.audio.as_ref().map_or(0, |a| a.sample_rate),
    }
}

/// Resolve one output file: its muxer, its streams and its codecs.
///
/// # Errors
///
/// A [`Diagnostic`] for an absent muxer, an empty stream list, an unknown
/// encoder, or a stream that needs an encoder this build does not have.
pub fn resolve_output(
    cli: &Cli,
    out: &OutputSpec,
    files: &[InputStreams],
) -> Result<ResolvedOutput, Diagnostic> {
    let format = muxer_for(out)?;
    let selection = select::resolve(files, &out.maps, out.blocked, &|_| true)?;

    let streams: Vec<OutStream> = selection
        .picks
        .iter()
        .map(|p| OutStream {
            source: *p,
            media: files
                .get(p.file as usize)
                .and_then(|f| f.streams.iter().find(|s| s.index == p.stream))
                .and_then(|s| s.media_type),
        })
        .collect();

    if selection.dropped {
        // Not an error and not an empty file: the reference creates nothing at
        // all and exits 0.
        return Ok(ResolvedOutput {
            index: out.index,
            url: out.url.clone(),
            format,
            streams: Vec::new(),
            sink: Sink::new(),
            dropped: true,
        });
    }

    if streams.is_empty() {
        return Err(Diagnostic::opening(
            AvError::EINVAL,
            vec![format!(
                "[out#{}/{format}] Output file does not contain any stream",
                out.index
            )],
            "output",
            &out.url,
        ));
    }

    check_codecs(cli, out, &streams)?;

    Ok(ResolvedOutput {
        index: out.index,
        url: out.url.clone(),
        format,
        streams,
        sink: Sink::new(),
        dropped: false,
    })
}

/// Which muxer an output resolves to. See the module docs for the three
/// refusals.
fn muxer_for(out: &OutputSpec) -> Result<&'static str, Diagnostic> {
    if let Some(name) = out.format.as_deref() {
        if crate::nullmux::NULL_MUXER.matches_name(name) {
            return Ok(crate::nullmux::NULL_MUXER.name);
        }
        if let Some(desc) = vaco_registry::muxer_by_name(name) {
            return Ok(desc.name);
        }
        if vaco_registry::demuxer_by_name(name).is_some() {
            return Err(no_muxer(out, name));
        }
        // Byte-identical to the reference modulo its log pointer.
        return Err(Diagnostic::opening(
            AvError::EINVAL,
            vec![
                format!("[AVFormatContext] Requested output format '{name}' is not known."),
                format!(
                    "[out#{}] Error initializing the muxer for {}: {}",
                    out.index,
                    out.url,
                    AvError::EINVAL.text
                ),
            ],
            "output",
            &out.url,
        ));
    }

    if let Some(desc) = vaco_registry::muxers_for_extension(&out.url).next() {
        return Ok(desc.name);
    }
    if let Some(desc) = vaco_registry::demuxers_for_extension(&out.url).next() {
        return Err(no_muxer(out, desc.name));
    }
    Err(Diagnostic::opening(
        AvError::EINVAL,
        vec![
            format!(
                "[AVFormatContext] Unable to choose an output format for '{}'; use a standard extension for the filename or specify the format manually.",
                out.url
            ),
            format!(
                "[out#{}] Error initializing the muxer for {}: {}",
                out.index,
                out.url,
                AvError::EINVAL.text
            ),
        ],
        "output",
        &out.url,
    ))
}

fn no_muxer(out: &OutputSpec, name: &str) -> Diagnostic {
    Diagnostic::opening(
        AvError::MUXER_NOT_FOUND,
        vec![
            format!(
                "[out#{}] No muxer for '{name}': this build reads that format but cannot write it. D5 scopes v0.1 to demuxing, so `-f null` is the only output.",
                out.index
            ),
            format!(
                "[out#{}] Error initializing the muxer for {}: {}",
                out.index,
                out.url,
                AvError::MUXER_NOT_FOUND.text
            ),
        ],
        "output",
        &out.url,
    )
}

/// Every output stream must be `-c copy`, because there are no encoders.
fn check_codecs(cli: &Cli, out: &OutputSpec, streams: &[OutStream]) -> Result<(), Diagnostic> {
    // `-c:v copy` is a per-*output*-stream option, so the specifier is matched
    // against the output's own stream list, not the input's. Building that view
    // is what makes `-c:a:1 flac -c:a copy` resolve the way the reference does
    // (last match wins, however specific the earlier one was).
    let view: Vec<StreamInfo> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| StreamInfo {
            index: i as u32,
            media_type: s.media,
            codec_known: true,
            ..StreamInfo::default()
        })
        .collect();
    let ctx = MatchCtx::streams(&view);
    let group = cli.output_group(out.index);

    for (i, s) in streams.iter().enumerate() {
        let chosen = group
            .and_then(|g| g.stream_option("c", &ctx, i as u32).ok().flatten())
            .and_then(|o| o.value.as_ref())
            .and_then(|v| v.to_str());
        match chosen {
            Some("copy") => {}
            Some(name) => {
                return Err(encoder_error(
                    out,
                    s,
                    i,
                    &format!("Unknown encoder '{name}'"),
                ));
            }
            None => {
                return Err(encoder_error(
                    out,
                    s,
                    i,
                    &format!(
                        "Automatic encoder selection failed Default encoder for format {} (codec none) is probably disabled. Please choose an encoder manually.",
                        out.format.as_deref().unwrap_or("null")
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn encoder_error(out: &OutputSpec, s: &OutStream, i: usize, detail: &str) -> Diagnostic {
    let tag = match s.media {
        Some(MediaType::Video) => "vost",
        Some(MediaType::Audio) => "aost",
        Some(MediaType::Subtitle) => "sost",
        _ => "dost",
    };
    let prefix = format!("[{tag}#{}:{i}]", out.index);
    Diagnostic::opening(
        AvError::ENCODER_NOT_FOUND,
        vec![
            format!("{prefix} {detail}"),
            format!("{prefix} Error selecting an encoder"),
        ],
        "output",
        &out.url,
    )
}

/// Build and run the pipeline.
///
/// Consumes the inputs, because `vaco-sched` takes each demuxer by value.
///
/// # Errors
///
/// A [`Diagnostic`] for a wiring failure the builder rejects, or for a pipeline
/// that stalls or is cancelled.
pub fn run_pipeline(
    inputs: Vec<InputFile>,
    outputs: &[ResolvedOutput],
    files: &[InputStreams],
) -> Result<RunSpec, Diagnostic> {
    if outputs.iter().all(|o| o.dropped) {
        // Nothing to write anywhere, so there is nothing to read either. The
        // reference does not open the output file at all in this case; reading
        // the inputs to end of stream and discarding every packet would be a
        // slow way of doing the same nothing.
        return Ok(RunSpec::default());
    }

    let params: Vec<Vec<(u32, vaco_codec_core::CodecParameters)>> = inputs
        .iter()
        .map(|f| {
            f.demuxer
                .streams()
                .iter()
                .map(|s| (s.index, s.params.clone()))
                .collect()
        })
        .collect();

    let mut spec = PipelineSpec::new();
    let mut refs = Vec::new();
    for f in inputs {
        refs.push(spec.add_input(f.demuxer));
    }

    let mut report = RunSpec::default();
    let mut sinks = Vec::new();
    for out in outputs.iter().filter(|o| !o.dropped) {
        let muxer = Box::new(NullMuxer::new(out.sink.clone()));
        // `add_output_with`, not `add_output`: the flags decide whether the
        // mux-side timestamp chain enforces strictly increasing DTS, and the
        // default of "no flags" means "strict". See `nullmux::FLAGS`.
        let oref = spec.add_output_with(
            muxer,
            crate::nullmux::FLAGS,
            vaco_format_core::options::FormatOptions::default(),
        );
        sinks.push(out.sink.clone());
        for (i, s) in out.streams.iter().enumerate() {
            let Some(input) = refs.get(s.source.file as usize).copied() else {
                return Err(internal("a map names an input that was not opened"));
            };
            let tap = spec
                .input_stream(input, s.source.stream)
                .map_err(|_| internal("a map names a stream the demuxer does not have"))?;
            let p = params
                .get(s.source.file as usize)
                .and_then(|v| v.iter().find(|(idx, _)| *idx == s.source.stream))
                .map_or_else(
                    || vaco_codec_core::CodecParameters::new(MediaType::Data),
                    |(_, p)| p.clone(),
                );
            spec.map(tap, oref, &p)
                .map_err(|e| internal_from("the muxer refused a stream", &e))?;
            report.mapping.push(format!(
                "  Stream #{}:{} -> #{}:{} (copy)",
                s.source.file, s.source.stream, out.index, i
            ));
        }
    }

    let mut pipeline = spec
        .build()
        .map_err(|e| internal_from("the pipeline could not be built", &e))?;
    // Serial, deliberately. Plan 12's PF-0.x record has five confident
    // performance predictions measuring backwards, the most recent a threading
    // design 45-60x slower than serial; a two-node demux-to-sink graph has
    // nothing to overlap, so the thread pool would be pure overhead. Revisit
    // with a measurement, not a hunch.
    let finish = Driver::serial().run(&mut pipeline).map_err(|e| {
        Diagnostic::new(AvError::of(&e), vec![format!("Error while filtering: {e}")])
    })?;

    match finish {
        Finish::Complete => {}
        Finish::Cancelled => {
            return Err(Diagnostic::new(
                AvError::EINVAL,
                vec!["Conversion cancelled".to_owned()],
            ));
        }
        Finish::Stalled(reports) => {
            let mut lines = vec!["Pipeline stalled; no node can make progress.".to_owned()];
            for r in reports {
                lines.push(format!("  {}: {}", r.label, r.reason));
            }
            return Err(Diagnostic::new(AvError::EINVAL, lines));
        }
    }

    for (out, sink) in outputs.iter().filter(|o| !o.dropped).zip(sinks) {
        let t = sink.tally();
        report.summary.push(summary_line(out, &t));
        report.tallies.push(t);
    }
    let _ = files;
    Ok(report)
}

/// The reference's end-of-run line, without the pointer it prints in the
/// prefix.
///
/// ```text
/// [out#0/null] video:7KiB audio:16KiB subtitle:0KiB other streams:0KiB global headers:0KiB muxing overhead: unknown
/// ```
///
/// The sizes **round**, they do not truncate, and that took a second file to
/// find. Measured by summing `ffprobe -show_entries packet=size` over the
/// selected streams and comparing with what `ffmpeg` printed for the same run:
///
/// | payload | ÷1024 | reference prints |
/// |---|---|---|
/// | 7 459 B video | 7.284 | `7KiB` |
/// | 16 354 B audio | 15.971 | **`16KiB`** |
/// | 8 992 B audio | 8.781 | **`9KiB`** |
///
/// Truncation gives 15 and 8 for the last two, so the first file alone would
/// have confirmed the wrong rule. It is a `printf` conversion of a double,
/// hence `round_ties_even` rather than `round`: a payload of exactly
/// *n*·1024 + 512 bytes is reachable and the two disagree there.
///
/// `muxing overhead` is `unknown` for `null` because nothing was written to
/// compare the payload against.
#[must_use]
pub fn summary_line(out: &ResolvedOutput, t: &OutputTally) -> String {
    let kib = |m: MediaType| kib_of(t.bytes_of(m));
    let other: u64 = t
        .streams
        .iter()
        .filter(|s| {
            !matches!(
                s.media,
                Some(MediaType::Video | MediaType::Audio | MediaType::Subtitle)
            )
        })
        .map(|s| s.bytes)
        .sum();
    format!(
        "[out#{}/{}] video:{}KiB audio:{}KiB subtitle:{}KiB other streams:{}KiB global headers:0KiB muxing overhead: unknown",
        out.index,
        out.format,
        kib(MediaType::Video),
        kib(MediaType::Audio),
        kib(MediaType::Subtitle),
        kib_of(other),
    )
}

/// Bytes as the reference's `%1.0f` kibibyte field renders them.
fn kib_of(bytes: u64) -> u64 {
    (bytes as f64 / 1024.0).round_ties_even() as u64
}

fn internal(what: &str) -> Diagnostic {
    Diagnostic::new(AvError::EINVAL, vec![format!("Internal error: {what}")])
}

fn internal_from(what: &str, e: &vaco_core::Error) -> Diagnostic {
    Diagnostic::new(AvError::of(e), vec![format!("Error: {what}: {e}")])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::cli;

    fn out_of(argv: &[&str]) -> (Cli, OutputSpec) {
        let c = cli::parse(argv).unwrap();
        let o = c.outputs.first().cloned().unwrap();
        (c, o)
    }

    #[test]
    fn dispositions_are_bit_aligned_between_the_two_crates() {
        // D19 lists `Disposition` as a tracked duplicate that is "aligned
        // numerically (19 flags, same bits) so nothing is wrong today". This
        // crate converts between them by copying the bits, so if that ever
        // stops being true the conversion silently mislabels every stream.
        // A compile-time-adjacent assertion, in the spirit of the
        // `BITSTREAM_PADDING` pattern D19 praises.
        for &(flag, name) in vaco_cli_core::Disposition::ALL {
            let other = vaco_format_core::Disposition::by_name(name)
                .unwrap_or_else(vaco_format_core::Disposition::empty);
            assert_eq!(flag.bits(), other.bits(), "{name}");
        }
    }

    #[test]
    fn f_null_resolves_to_the_null_muxer() {
        let (_, o) = out_of(&["-i", "a.mkv", "-f", "null", "-"]);
        assert_eq!(muxer_for(&o).unwrap(), "null");
    }

    /// A format this build demuxes and does not mux, or `None` if every
    /// demuxable format is now also muxable.
    ///
    /// The tests below used to name `matroska`, which was demux-only when they
    /// were written. That is a fact about one moment, and a test that pins one
    /// fails the day the gap it describes is closed — which is the least useful
    /// day for a test to fail. What is actually invariant is the *wording*:
    /// whenever such a format exists, saying so beats reporting it unknown.
    fn a_demux_only_format() -> Option<&'static str> {
        vaco_registry::demuxers()
            .iter()
            .map(|d| d.name)
            .find(|n| vaco_registry::muxer_by_name(n).is_none())
    }

    #[test]
    fn a_readable_format_says_so_rather_than_pretending_it_is_unknown() {
        let Some(name) = a_demux_only_format() else {
            return;
        };
        let (_, o) = out_of(&["-i", "a.mkv", "-f", name, "out.bin"]);
        let e = muxer_for(&o).unwrap_err();
        assert!(
            e.render().contains("reads that format but cannot write it"),
            "{name}: {}",
            e.render()
        );
        assert!(
            e.render()
                .ends_with("Error opening output files: Muxer not found\n")
        );
        assert_eq!(e.exit.code(), 8);
    }

    #[test]
    fn an_unknown_format_keeps_the_reference_wording_and_status() {
        let (_, o) = out_of(&["-i", "a.mkv", "-f", "nosuchformat", "-"]);
        let e = muxer_for(&o).unwrap_err();
        assert!(
            e.render().starts_with(
                "[AVFormatContext] Requested output format 'nosuchformat' is not known.\n"
            ),
            "{}",
            e.render()
        );
        assert_eq!(e.exit.code(), 234);
    }

    #[test]
    fn an_unhelpful_extension_keeps_the_reference_wording_and_status() {
        let (_, o) = out_of(&["-i", "a.mkv", "out.zzz"]);
        let e = muxer_for(&o).unwrap_err();
        assert!(
            e.render().starts_with(
                "[AVFormatContext] Unable to choose an output format for 'out.zzz'; use a standard extension"
            ),
            "{}",
            e.render()
        );
        assert_eq!(e.exit.code(), 234);
    }

    #[test]
    fn an_extension_this_build_can_read_says_so() {
        // Chosen from the registry for the same reason as above: an extension
        // that only demuxes today may well mux tomorrow.
        let Some((ext, _)) = vaco_registry::demuxers()
            .iter()
            .filter(|d| vaco_registry::muxer_by_name(d.name).is_none())
            .find_map(|d| d.extensions.first().map(|e| (*e, d.name)))
            .filter(|(e, _)| {
                vaco_registry::muxers_for_extension(&format!("x.{e}"))
                    .next()
                    .is_none()
            })
        else {
            return;
        };
        let (_, o) = out_of(&["-i", "a.mkv", &format!("out.{ext}")]);
        let e = muxer_for(&o).unwrap_err();
        assert!(
            e.render().contains("cannot write it"),
            "{ext}: {}",
            e.render()
        );
    }

    #[test]
    fn a_stream_with_no_codec_takes_the_reference_missing_encoder_path() {
        let (c, mut o) = out_of(&["-i", "a.mkv", "-f", "null", "-"]);
        o.format = Some("null".to_owned());
        let s = OutStream {
            source: StreamPick { file: 0, stream: 0 },
            media: Some(MediaType::Video),
        };
        let e = check_codecs(&c, &o, &[s]).unwrap_err();
        assert_eq!(
            e.render(),
            "[vost#0:0] Automatic encoder selection failed Default encoder for format null (codec none) is probably disabled. Please choose an encoder manually.\n\
             [vost#0:0] Error selecting an encoder\n\
             Error opening output file -.\n\
             Error opening output files: Encoder not found\n"
        );
        assert_eq!(e.exit.code(), 8);
    }

    #[test]
    fn copy_is_accepted_and_a_named_encoder_is_not() {
        let (c, o) = out_of(&["-i", "a.mkv", "-c", "copy", "-f", "null", "-"]);
        let s = OutStream {
            source: StreamPick { file: 0, stream: 0 },
            media: Some(MediaType::Video),
        };
        assert!(check_codecs(&c, &o, &[s]).is_ok());

        let (c, o) = out_of(&["-i", "a.mkv", "-c:v", "libx264", "-f", "null", "-"]);
        let e = check_codecs(&c, &o, &[s]).unwrap_err();
        assert!(
            e.render()
                .starts_with("[vost#0:0] Unknown encoder 'libx264'\n"),
            "{}",
            e.render()
        );
        assert_eq!(e.exit.code(), 8);
    }

    #[test]
    fn last_match_wins_across_per_stream_codec_options() {
        // `-c:a:1 flac -c:a copy` gives stream a:1 `copy`, not `flac`.
        let (c, o) = out_of(&[
            "-i", "a.mkv", "-c:a:1", "flac", "-c:a", "copy", "-f", "null", "-",
        ]);
        let streams = vec![
            OutStream {
                source: StreamPick { file: 0, stream: 0 },
                media: Some(MediaType::Audio),
            },
            OutStream {
                source: StreamPick { file: 0, stream: 1 },
                media: Some(MediaType::Audio),
            },
        ];
        assert!(check_codecs(&c, &o, &streams).is_ok());
    }

    #[test]
    fn the_summary_line_rounds_to_whole_kibibytes() {
        let out = ResolvedOutput {
            index: 0,
            url: "-".to_owned(),
            format: "null",
            streams: Vec::new(),
            sink: Sink::new(),
            dropped: false,
        };
        let t = OutputTally {
            streams: vec![
                crate::nullmux::StreamTally {
                    media: Some(MediaType::Video),
                    packets: 1,
                    bytes: 7459,
                },
                crate::nullmux::StreamTally {
                    media: Some(MediaType::Audio),
                    packets: 1,
                    bytes: 16_354,
                },
            ],
            header_written: true,
            trailer_written: true,
        };
        assert_eq!(
            summary_line(&out, &t),
            "[out#0/null] video:7KiB audio:16KiB subtitle:0KiB other streams:0KiB global headers:0KiB muxing overhead: unknown"
        );
        // The three measured points, and the tie the two rounding rules
        // disagree on.
        assert_eq!(kib_of(7459), 7);
        assert_eq!(kib_of(16_354), 16);
        assert_eq!(kib_of(8992), 9);
        assert_eq!(kib_of(1536), 2, "1.5 KiB ties to even");
        assert_eq!(kib_of(2560), 2, "2.5 KiB ties to even");
        assert_eq!(kib_of(0), 0);
    }
}
