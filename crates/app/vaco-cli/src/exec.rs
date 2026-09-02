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
//! open the real sink    (this module)   -> bytes on disk, or a diagnosis
//! ```
//!
//! # The registry has 63 muxers now; this module reaches all of them
//!
//! This used to say the opposite: D5 put zero muxers in v0.1, `crates/format/`
//! held three `vaco-demux-*` crates and no `vaco-mux-*`, and [`muxer_for`]
//! could return exactly one thing — a local, unregistered `null` sink. That
//! stopped being true when the container wave landed a `vaco-mux-*` crate per
//! format and registered `null` itself as a real component
//! (`vaco_mux_utility::MUXER_NULL`). [`muxer_for`] now resolves **every**
//! `-f`/extension through `vaco_registry::muxer_by_name` /
//! `muxers_for_extension` uniformly — `null` included, no special case — and
//! [`run_pipeline`] opens whatever it names through the real protocol and
//! format-registry stack (`crate::output`, then `(MuxerDesc::open)`) rather
//! than always building a discard sink. The refusal path still distinguishes
//! three cases, and it is what is left of the old design:
//!
//! | `-f` / extension | message | exit |
//! |---|---|---|
//! | a format this build can **read** but has no registered muxer for | "no muxer for 'x': this build reads that format but cannot write it" | 8 |
//! | a name nothing claims (`-f nosuchformat`) | the reference's own "Requested output format 'x' is not known." | 234 |
//! | no `-f` and an unhelpful extension | the reference's own "Unable to choose an output format for 'x'…" | 234 |
//!
//! The second and third are byte-identical to `ffmpeg` 8.1 modulo the pointer
//! it prints in its log prefix. The first now applies to *fewer* formats than
//! it used to — every format with a landed `vaco-mux-*` crate moved out of it —
//! but it is still correct for whatever remains: `AVERROR_MUXER_NOT_FOUND` is
//! the code that names "this build demuxes it and cannot mux it", and it exits
//! 8 like every other four-character tag.
//!
//! # There are still no encoders
//!
//! So an output stream must be `-c copy`. Without one, the run takes the
//! reference's *own* path for a build missing an encoder — "Default encoder for
//! format null (codec none) is probably disabled" — which is a message it
//! already emits for exactly this situation and which is therefore the right
//! one to reproduce rather than invent. Stream copy is enough to remux,
//! though, which is the one thing this module is now actually for.
//!
//! # CL-16: `-metadata`, `-map_metadata`, `-map_chapters`, and FW-11
//!
//! [`metadata_of`] resolves `-metadata`/`-metadata:s:…` against an output's
//! own stream list into a [`vaco_format_core::metadata::MuxMetadata`];
//! [`resolve_mapped_metadata`] finishes it with `-map_chapters`/
//! `-map_metadata`'s source input once one is actually open; [`run_pipeline`]
//! hands the result to [`vaco_sched::spec::PipelineSpec::set_output_metadata`]
//! *after* [`PipelineSpec::add_output_with`], which is now safe to do in that
//! order: `PipelineSpec::build` delivers it to [`Muxer::set_metadata`] at
//! `MuxBuilder::open` (M30), after every stream is declared and its time base
//! settled, but before the header.

//! That was not always true: `vaco-sched`'s `MuxWork` used to drive a raw
//! `dyn Muxer`, so this
//! module used to call [`Muxer::set_metadata`] on the freshly opened muxer
//! directly, *before* handing it to `add_output_with` — there was no later
//! point at which it could still reach the muxer. `vaco-mux-mp4` and
//! `vaco-mux-matroska`'s `ffmetadata` resolve per-stream fields lazily, at
//! `write_header` time, to survive exactly that ordering (see each crate's
//! module docs); that laziness is no longer load-bearing for this path, but
//! neither crate is this one's to change.
//!
//! `-metadata:c:N`/`-metadata:p:N` parse (a malformed one still reports the
//! reference's own specifier error) but land nowhere: chapter-scoped tag
//! overrides are not implemented, and a *program*'s metadata has no
//! representation in `MuxMetadata` at all. **`-disposition` and `-program`
//! are parsed by `vaco-cli-core`'s option tables but not resolved here** —
//! `Muxer::add_stream` takes only `CodecParameters`, which carries neither a
//! disposition bit nor a program membership, so writing either one needs a
//! channel this trait does not have, the same class of gap `MuxMetadata`
//! closed for tags/chapters/attachments (gap 1) but does not itself cover.
//! Reported rather than worked around, per this crate's brief.
//!
//! `add_output_with` (not the plain `add_output`) carries the output side's
//! [`vaco_format_core::FormatOptions`] (FW-11: `-avoid_negative_ts`,
//! `-max_interleave_delta`, …) into the scheduler; the input side reaches
//! [`vaco_format_core::Demuxer::reconfigure`] through
//! [`crate::input::OpenRequest::format_opts`] instead, since discovery runs
//! long before any `PipelineSpec` exists.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vaco_cli_core::{MatchCtx, MetadataSpecifier, StreamInfo};
use vaco_core::{Disposition, Error, MediaType, Result};
use vaco_expr::Bindings;
use vaco_filter_core::LinkFormat;
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::{
    Muxer, Stream, dihedral_transform_from_angle_and_flips, dihedral_transform_from_matrix,
};
use vaco_codec_core::CodecId;
use vaco_pixfmt::PixFmt;
use vaco_sched::{Driver, Finish, PipelineSpec, SourceBind};

use crate::cli::{Cli, OutputSpec, value_str};
use crate::complexgraph::{self, ComplexPad};
use crate::exit::{AvError, Diagnostic};
use crate::input::InputFile;
use crate::nullmux::{OutputTally, Sink, TallyingMuxer};
use crate::select::{self, InputStreams, StreamPick};

/// One resolved output stream, in output order.
#[derive(Debug, Clone)]
pub struct OutStream {
    pub source: StreamPick,
    pub media: Option<MediaType>,
    /// What `-c`/`-c:v`/`-c:a` resolved to for this stream. [`check_codecs`]
    /// is where this is decided; [`run_pipeline`] is where it is acted on.
    pub codec: StreamCodec,
    /// CL-20: `-vf`/`-af`/`-filter`, `-s`, `-aspect`, `-pix_fmt` for this
    /// stream. [`graph_options_of`] is where this is decided;
    /// [`run_pipeline`] is where it is acted on — and only for a stream that
    /// is actually being transcoded, per [`graph_options_of`]'s own
    /// streamcopy-conflict check.
    pub graph_opts: crate::filtergraph::SimpleGraphOptions,
    /// CL-22: `-force_key_frames`, parsed and validated here so a malformed
    /// value is a real, early error — the same treatment `-s`/`-pix_fmt`
    /// already get — but not yet acted on. See
    /// `crate::force_key_frames`'s module doc for the seam this needs that
    /// does not exist in this build (a per-frame node in `vaco-sched`) and
    /// for why nothing this build can encode would show a difference even
    /// with one (every registered video encoder is intra-only).
    pub force_key_frames: Option<crate::force_key_frames::ForceKeyFrames>,
    /// The generic encoder options this stream's `-b`/`-qscale` (any of their
    /// aliases: `-b:v`, `-vb`, `-ab`, `-q`, `-qscale`, `-aq`) resolved to,
    /// as `(key, value)` pairs already evaluated to a plain number —
    /// [`codec_options_of`] is where this is decided; [`run_pipeline`] is
    /// where it is fed to [`vaco_codec_core::Encoder::set_option`], the
    /// channel that closes the "VP8's rate control is unreachable from the
    /// command line" gap. Empty for a stream that names neither option, and
    /// for one resolved to [`StreamCodec::Copy`] (nothing to configure).
    pub codec_options: Vec<(String, String)>,
    /// The display matrix this stream's *output* container should carry —
    /// [`output_display_matrix`] is where this is decided (it needs
    /// [`OutStream::graph_opts`]'s `rotate`, already resolved by that
    /// point, to know whether a rotation was baked into the pixels);
    /// [`run_pipeline`] passes it to
    /// [`vaco_sched::spec::PipelineSpec::map_with_matrix`] for every
    /// stream, copied or encoded alike, since a copy never reaches the
    /// per-stream [`Self::graph_opts`] path a transcode does.
    pub output_matrix: Option<[i32; 9]>,
    /// `-bsf`/`-bsf:v`/`-bsf:a`/`-bsf:s` for this stream, already resolved
    /// against the registry (a typo is refused here, not deep inside a
    /// running mux) and validated to carry no per-instance options (gap 12).
    /// [`bsf_options_of`] is where this is decided; [`run_pipeline`] hands it
    /// to [`vaco_sched::spec::PipelineSpec::set_output_stream_bsf`] right
    /// after `map_with_matrix` returns the real stream index this needs.
    /// Empty for a stream that names no `-bsf` at all — the common case,
    /// and indistinguishable from "not yet resolved" only before this field
    /// is set, never after.
    pub bsf: Vec<vaco_format_core::mux::UserBsf>,
}

/// What `-c` resolved to for one output stream: pass packets
/// through unchanged, or decode then encode through a specific registered
/// encoder. `Copy` is the field's default only in the sense that
/// [`resolve_output`] has to put *something* in [`OutStream`] before
/// [`check_codecs`] has run; every stream this crate actually builds a
/// pipeline leg for has had this decided by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCodec {
    Copy,
    /// The registered encoder's own name, e.g. `"qoi"` — not necessarily
    /// byte-identical to what `-c:v` was given, the same way
    /// `vaco_registry::decoder_by_name` normalises nothing today but a future
    /// alias table would read off the descriptor rather than off the
    /// argument.
    Encode(&'static str),
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
    /// CL-16: file- and stream-level `-metadata`/`-metadata:s:…`, already
    /// resolved against this output's own stream list. `chapters` is left
    /// empty here — [`run_pipeline`] fills it from `-map_chapters`'s source
    /// input, which this function never sees (see that function's docs).
    pub metadata: MuxMetadata,
    /// `-map_chapters <n>` ([`crate::cli::OutputSpec::map_chapters`]).
    pub map_chapters: Option<i64>,
    /// `-map_metadata <n>` ([`crate::cli::OutputSpec::map_metadata`]).
    pub map_metadata: Option<i64>,
    /// FW-11: the output side's generic `AVFormatContext` options
    /// (`-avoid_negative_ts`, `-max_interleave_delta`, …), fed to
    /// [`vaco_sched::spec::PipelineSpec::add_output_with`] in [`run_pipeline`].
    pub format_opts: vaco_format_core::FormatOptions,
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
    /// Parallel to [`RunSpec::tallies`]: the real muxed byte count
    /// [`summary_line`] used for `muxing overhead`, kept alongside it for
    /// `-stats`'s `Lsize=` (CL-17, `crate::stats`) so that field does not
    /// need its own pass over the outputs to recompute what this one already
    /// knows. `None` for a `NOFILE` container, same meaning as `unknown`
    /// there.
    pub total_bytes: Vec<Option<u64>>,
    /// The longest input's stated duration in seconds, if any input states
    /// one. `-stats`'s `time=` field approximates the muxed presentation-time
    /// range with this, because [`crate::nullmux::StreamTally`] carries
    /// packet and byte counts and not timestamps — see `crate::stats`'s
    /// module docs for why that is a documented approximation rather than a
    /// measured field.
    pub input_duration_secs: Option<f64>,
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
        display_matrix: streams.iter().map(display_matrix_of).collect(),
    }
}

/// `-display_rotation`/`-display_hflip`/`-display_vflip` (each `INPUT,
/// PER_STREAM`, overriding the container's own matrix wholesale -- measured,
/// `ffmpeg 9.0.1 -h full`) plus `-autorotate` (default on) resolved for one
/// output stream's *source* input stream, or `None` if there is nothing to
/// rotate: no matrix, an identity one, an angle that is not a clean multiple
/// of 90° (see [`vaco_format_core::DisplayTransform`]'s own doc), a
/// `-filter_complex`/`-lavfi` source (no container stream to read a matrix
/// from), or `-noautorotate`.
///
/// # Errors
///
/// The specifier grammar's error (a malformed `-display_rotation:v:9`), or
/// an invalid `-display_rotation` value.
fn stream_opt<'a>(
    group: Option<&'a vaco_cli_core::OptionGroup>,
    ctx: &MatchCtx<'_>,
    stream: u32,
    name: &str,
) -> Result<Option<&'a vaco_cli_core::ParsedOption>, Diagnostic> {
    match group {
        Some(g) => g
            .stream_option(name, ctx, stream)
            .map_err(|e| crate::cli::split_error(&e)),
        None => Ok(None),
    }
}

/// The raw display matrix that governs one demuxed output stream's
/// rotation: an explicit `-display_rotation`/`-display_hflip`/
/// `-display_vflip` override if any of the three is given (built through
/// [`vaco_format_core::DisplayTransform::to_matrix`], so only the eight
/// recognised dihedral cases are representable this way — see
/// [`vaco_format_core::DisplayTransform`]'s own doc for why an arbitrary
/// angle is not), else the input's own container matrix as-is (even one
/// this build cannot decompose, so a caller that only wants to *preserve*
/// it — [`output_display_matrix`] — does not need it to be one of the
/// eight), else `None`.
///
/// `Ok(None)` for a `-filter_complex`/`-lavfi`-sourced stream (no input
/// stream to read anything from) or an unresolvable specifier.
///
/// # Errors
///
/// The specifier grammar's error, or an invalid `-display_rotation` value.
fn effective_matrix(
    cli: &Cli,
    files: &[select::InputStreams],
    source: StreamPick,
) -> Result<Option<[i32; 9]>, Diagnostic> {
    let StreamPick::Demuxed { file, stream } = source else {
        return Ok(None);
    };
    let Some(input) = files.get(file as usize) else {
        return Ok(None);
    };
    let ctx = MatchCtx::streams(&input.streams);
    let group = cli.input_group(file);

    let rotation_opt = stream_opt(group, &ctx, stream, "display_rotation")?;
    let hflip_opt = stream_opt(group, &ctx, stream, "display_hflip")?;
    let vflip_opt = stream_opt(group, &ctx, stream, "display_vflip")?;

    if rotation_opt.is_some() || hflip_opt.is_some() || vflip_opt.is_some() {
        let degrees = rotation_opt
            .map(|o| o.number().map_err(|e| crate::cli::split_error(&e)))
            .transpose()?
            .flatten()
            .unwrap_or(0.0);
        return Ok(dihedral_transform_from_angle_and_flips(
            degrees,
            hflip_opt.is_some(),
            vflip_opt.is_some(),
        )
        .map(vaco_format_core::DisplayTransform::to_matrix));
    }
    Ok(input
        .streams
        .iter()
        .position(|s| s.index == stream)
        .and_then(|pos| input.display_matrix.get(pos).copied().flatten()))
}

/// The [`vaco_format_core::DisplayTransform`] `-autorotate` (default on;
/// `-noautorotate` disables) says to actually bake into this output
/// stream's decoded pixels, or `None` if there is nothing to rotate, the
/// matrix does not decompose into one of the eight recognised cases (see
/// [`vaco_format_core::DisplayTransform`]'s own doc), or `-noautorotate`
/// was given.
///
/// # Errors
///
/// [`effective_matrix`]'s.
fn resolve_rotate(
    cli: &Cli,
    files: &[select::InputStreams],
    source: StreamPick,
) -> Result<Option<vaco_format_core::DisplayTransform>, Diagnostic> {
    let StreamPick::Demuxed { file, stream } = source else {
        return Ok(None);
    };
    let ctx = files
        .get(file as usize)
        .map(|input| MatchCtx::streams(&input.streams));
    let Some(ctx) = ctx else {
        return Ok(None);
    };
    let group = cli.input_group(file);
    let autorotate_on = !stream_opt(group, &ctx, stream, "autorotate")?.is_some_and(|o| o.negated);
    if !autorotate_on {
        return Ok(None);
    }
    let matrix = effective_matrix(cli, files, source)?;
    Ok(matrix.and_then(|m| dihedral_transform_from_matrix(&m)))
}

/// The display matrix one output stream's *container* should carry —
/// interface gap 22c's muxer half. `rotate_applied` is whatever
/// [`resolve_rotate`] already decided for the *same* stream: when it baked
/// a rotation into the decoded pixels, the output must be identity or the
/// next player rotates the file twice; otherwise (a `-c copy` stream, which
/// never has a [`resolve_rotate`] call at all, or a transcode with
/// `-noautorotate` or a non-decomposable matrix) the source's own
/// [`effective_matrix`] is passed straight through, so the file still
/// carries the orientation hint it arrived with.
///
/// # Errors
///
/// [`effective_matrix`]'s.
fn output_display_matrix(
    cli: &Cli,
    files: &[select::InputStreams],
    source: StreamPick,
    rotate_applied: bool,
) -> Result<Option<[i32; 9]>, Diagnostic> {
    if rotate_applied {
        return Ok(None);
    }
    effective_matrix(cli, files, source)
}

fn channels_of(s: &Stream) -> u32 {
    s.params
        .audio
        .as_ref()
        .and_then(|a| a.layout.as_ref())
        .map_or(0, |l| l.channels)
}

/// The container's own display transformation matrix, if this stream
/// carries one -- see `crate::select::InputStreams::display_matrix`.
fn display_matrix_of(s: &Stream) -> Option<[i32; 9]> {
    s.side_data.first().map(|d| {
        let vaco_format_core::StreamSideData::DisplayMatrix(m) = d;
        *m
    })
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
/// `complex`/`used_complex` are the whole invocation's flat catalog of
/// labelled `-filter_complex`/`-lavfi` output pads (CL-25) and which of them
/// earlier output files already consumed — see [`select::resolve`]'s docs.
///
/// # Errors
///
/// A [`Diagnostic`] for an absent muxer, an empty stream list, an unknown
/// encoder, a stream that needs an encoder this build does not have, a
/// `-map [label]` naming no open (or already-used) complex-graph output, or
/// `-c copy` on a stream fed by one (streamcopy and a complex filtergraph
/// cannot be combined — measured, `ffmpeg 8.1`).
#[allow(
    clippy::implicit_hasher,
    reason = "internal wiring type, not a public hashing surface"
)]
pub fn resolve_output(
    cli: &Cli,
    out: &OutputSpec,
    files: &[InputStreams],
    complex: &[ComplexPad],
    used_complex: &mut HashSet<usize>,
) -> Result<ResolvedOutput, Diagnostic> {
    let format = muxer_for(out)?;
    let selection = select::resolve(
        files,
        &out.maps,
        out.blocked,
        &|_| true,
        complex,
        used_complex,
    )?;

    let mut streams: Vec<OutStream> = selection
        .picks
        .iter()
        .map(|p| OutStream {
            source: *p,
            media: match p {
                StreamPick::Demuxed { file, stream } => files
                    .get(*file as usize)
                    .and_then(|f| f.streams.iter().find(|s| s.index == *stream))
                    .and_then(|s| s.media_type),
                StreamPick::Complex(index) => complex.get(*index).map(|c| c.media),
            },
            // Placeholder until `check_codecs` below decides it; every branch
            // that returns `streams` to a caller has run that check first.
            codec: StreamCodec::Copy,
            // Placeholder until `graph_options_of` below decides it, same as
            // `codec` above.
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            // Placeholder until `force_key_frames_of` below decides it, same
            // reason as `codec`/`graph_opts` above.
            force_key_frames: None,
            // Placeholder until `codec_options_of` below decides it, same
            // reason as `force_key_frames` above.
            codec_options: Vec::new(),
            // Placeholder until the `graph_opts`/`output_matrix` loop below
            // decides it -- needs `graph_opts.rotate`, resolved in that same
            // loop, so it cannot be decided any earlier than `graph_opts`
            // itself is.
            output_matrix: None,
            // Placeholder until `bsf_options_of` below decides it, same
            // reason as `codec_options` above.
            bsf: Vec::new(),
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
            metadata: MuxMetadata::default(),
            map_chapters: out.map_chapters,
            map_metadata: out.map_metadata,
            format_opts: out.format_opts.clone(),
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

    let codecs = check_codecs(cli, out, format, &streams)?;
    for (s, c) in streams.iter_mut().zip(codecs) {
        s.codec = c;
    }

    for (i, s) in streams.iter().enumerate() {
        if matches!(s.source, StreamPick::Complex(_)) && matches!(s.codec, StreamCodec::Copy) {
            return Err(complex_streamcopy_conflict(out, s, i));
        }
    }

    // CL-20: resolved after `codec`, because the streamcopy/filtergraph
    // conflict check needs to know which streams are actually being
    // transcoded.
    let graph_opts = graph_options_of(cli, out, &streams, files)?;
    for (i, (s, g)) in streams.iter_mut().zip(graph_opts).enumerate() {
        // CL-25: a further `-vf`/`-af` layered on top of a `-map [label]`
        // stream is not supported — see `crate::complexgraph`'s module docs
        // for why building that graph would need a `CodecParameters` a
        // complex-graph tap does not carry the same way a demuxed stream
        // does.
        if matches!(s.source, StreamPick::Complex(_)) && g.wants_graph() {
            return Err(Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "[{}#{}:{i}] a further filtergraph on a complex filtergraph output is not supported yet",
                    stream_tag(s.media),
                    out.index
                )],
            ));
        }
        s.graph_opts = g;
        // A resolved `rotate` never actually runs for a `StreamCodec::Copy`
        // stream: the pipeline's own `StreamCodec::Copy => (tap, p, "copy")`
        // arm (below) skips the whole `graph_opts.wants_graph()` block that
        // would apply it. Treating `graph_opts.rotate.is_some()` alone as
        // "already applied" made a `-c copy` of a rotated file write an
        // identity matrix — silently dropping the rotation hint exactly the
        // way the un-plumbed muxer used to, just one layer later. Only a
        // stream that is actually going to be filtered counts as applied.
        let rotate_applied = s.graph_opts.rotate.is_some() && !matches!(s.codec, StreamCodec::Copy);
        s.output_matrix = output_display_matrix(cli, files, s.source, rotate_applied)?;
    }

    let forced = force_key_frames_of(cli, out, &streams)?;
    for (s, f) in streams.iter_mut().zip(forced) {
        s.force_key_frames = f;
    }

    let codec_opts = codec_options_of(cli, out, &streams)?;
    for (s, o) in streams.iter_mut().zip(codec_opts) {
        s.codec_options = o;
    }

    let bsf_chains = bsf_options_of(cli, out, &streams)?;
    for (s, b) in streams.iter_mut().zip(bsf_chains) {
        s.bsf = b;
    }

    let metadata = metadata_of(cli, out, &streams)?;

    Ok(ResolvedOutput {
        index: out.index,
        url: out.url.clone(),
        format,
        streams,
        sink: Sink::new(),
        dropped: false,
        metadata,
        map_chapters: out.map_chapters,
        map_metadata: out.map_metadata,
        format_opts: out.format_opts.clone(),
    })
}

/// Build the `MuxMetadata` this output's `-metadata`/`-metadata:s:…` options
/// describe (CL-16), resolved against the output's *own* stream list — the
/// same `MatchCtx` shape [`check_codecs`] builds for `-c`, reused here for
/// the same reason: `-metadata:s:v:0` counts within the output, not the
/// input, the same way `-c:v:0` does.
///
/// `-metadata:c:N` and `-metadata:p:N` are parsed (so a malformed one still
/// reports the reference's own specifier error) but have nowhere to land: a
/// chapter's own metadata is filled in by [`run_pipeline`] from whichever
/// input `-map_chapters` names, at a point this function cannot see, and a
/// *program*'s metadata has no representation in
/// [`vaco_format_core::metadata::MuxMetadata`] at all -- the same shape of
/// gap [`vaco_format_core::metadata::MuxMetadata`] closes for
/// tags/chapters/attachments, just not here.
fn metadata_of(
    cli: &Cli,
    out: &OutputSpec,
    streams: &[OutStream],
) -> Result<MuxMetadata, Diagnostic> {
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

    let mut meta = MuxMetadata {
        stream_tags: vec![Vec::new(); streams.len()],
        ..MuxMetadata::default()
    };
    let Some(group) = cli.output_group(out.index) else {
        return Ok(meta);
    };

    for opt in &group.opts {
        if opt.resolved().0 != "metadata" {
            continue;
        }
        let spec = opt
            .metadata_spec()
            .map_err(|e| {
                Diagnostic::new(
                    AvError::EINVAL,
                    vec![format!("Invalid metadata specifier: {e}")],
                )
            })?
            .unwrap_or(MetadataSpecifier::Global);
        let raw = crate::cli::value_str(opt)?;
        // Measured (reference, `-metadata key` with no `=` at all): the whole
        // string becomes the key with an empty value, which deletes it —
        // matching `-metadata key=`'s own deletion rule below.
        let (key, value) = raw.split_once('=').unwrap_or((raw.as_str(), ""));
        match spec {
            MetadataSpecifier::Global => set_tag(&mut meta.tags, key, value),
            MetadataSpecifier::Stream(s) => {
                for (i, slot) in meta.stream_tags.iter_mut().enumerate() {
                    if s.matches(&ctx, i as u32) {
                        set_tag(slot, key, value);
                    }
                }
            }
            // Deferred — see this function's docs.
            MetadataSpecifier::Chapter(_) | MetadataSpecifier::Program(_) => {}
        }
    }

    // CL-16: `-attach <filename>`, one real `MuxAttachment` per occurrence, in
    // argv order — `vaco-mux-matroska` writes these onto a real `Attachments`/
    // `AttachedFile` element (confirmed by reading `mux.rs`'s own
    // `attachments_bytes`), so this is a real, working sink and not a value
    // with nowhere to land.
    for path in &out.attach {
        let data = std::fs::read(path).map_err(|e| {
            Diagnostic::opening(
                AvError::of(&Error::Io(e)),
                vec![format!(
                    "[out#{}/{format}] Could not open attachment file {path}.",
                    out.index,
                    format = out.format.as_deref().unwrap_or("?")
                )],
                "output",
                &out.url,
            )
        })?;
        let filename = std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        meta.attachments
            .push(vaco_format_core::metadata::MuxAttachment {
                filename,
                data,
                ..vaco_format_core::metadata::MuxAttachment::default()
            });
    }

    // `-metadata:s:t:N mimetype=…` — the reference's own way of naming an
    // attachment's media type (measured: `ffmpeg -attach f -metadata:s:t
    // mimetype=text/plain`). Attachments are not real output streams in this
    // build's model (unlike the reference's own CLI, which synthesises one),
    // so this reads `StreamSpecifier`'s `media`/`index` fields directly
    // rather than going through `MatchCtx`'s per-real-stream matching above.
    for opt in &group.opts {
        if opt.resolved().0 != "metadata" {
            continue;
        }
        let Ok(Some(MetadataSpecifier::Stream(spec))) = opt.metadata_spec() else {
            continue;
        };
        if spec.media != Some(vaco_cli_core::SpecMediaKind::Attachment) {
            continue;
        }
        let Some(index) = spec.index.and_then(|i| usize::try_from(i).ok()) else {
            continue;
        };
        let raw = value_str(opt)?;
        let (key, value) = raw.split_once('=').unwrap_or((raw.as_str(), ""));
        if key.eq_ignore_ascii_case("mimetype")
            && let Some(att) = meta.attachments.get_mut(index)
        {
            value.clone_into(&mut att.mime_type);
        }
    }

    // `-disposition[:stream_spec]`, resolved the same per-stream way
    // `codec_options_of` resolves `-b`/`-q` (last match wins). The value
    // lands on `MuxMetadata::stream_disposition` rather than on
    // `vaco_format_core::StreamSpec`: `add_stream`/`add_stream_with` run
    // before every `-disposition` occurrence for a stream is necessarily
    // known, but `MuxMetadata` is not read until `MuxBuilder::open`, after
    // every stream and every option on the command line exists.
    let mut stream_disposition = vec![Disposition::empty(); streams.len()];
    for (i, slot) in stream_disposition.iter_mut().enumerate() {
        if let Ok(Some(opt)) = group.stream_option("disposition", &ctx, i as u32) {
            let raw = value_str(opt)?;
            *slot = eval_disposition(&raw).map_err(|e| {
                Diagnostic::new(
                    AvError::EINVAL,
                    vec![format!(
                        "Error parsing option 'disposition' with value '{raw}': {e}"
                    )],
                )
            })?;
        }
    }
    meta.stream_disposition = stream_disposition;

    // `-program`, one `vaco_format_core::Program` per occurrence, in argv
    // order — the same "collect every occurrence" shape `-attach` above
    // uses, since a file may declare more than one. No muxer reads this yet
    // (see `MuxMetadata::programs`'s own doc); parsed and validated here so
    // a malformed value is a real, early diagnostic rather than a silent
    // "unrecognized option" or a value with nowhere to land unchecked.
    let mut programs = Vec::new();
    for opt in &group.opts {
        if opt.resolved().0 != "program" {
            continue;
        }
        let raw = value_str(opt)?;
        let mut program = parse_program(&raw).map_err(|e| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Error parsing option 'program' with value '{raw}': {e}"
                )],
            )
        })?;
        program.id = i64::try_from(programs.len()).unwrap_or(i64::MAX);
        programs.push(program);
    }
    meta.programs = programs;

    Ok(meta)
}

/// Evaluate a `-disposition` value the way the reference does: through the
/// same expression evaluator every numeric option uses, with every named
/// flag (`"default"`, `"forced"`, …) bound to its bit value — not a `+`/`-`
/// flag-list grammar of this crate's own invention. `vaco_core::Disposition`'s
/// own module doc found this measured, not assumed: the reference's error
/// message for an unknown name (`"Undefined constant or missing '('"`) is the
/// expression evaluator's own, and `-disposition:v:0 DEFAULT` (wrong case) is
/// rejected the same way `-crf` rejects an unknown identifier.
///
/// So `"default+original"` evaluates as plain arithmetic — `1.0 + 4.0` — and
/// the result is truncated to the flag bits actually named, exactly as an
/// `AVOption` of type `FLAGS` truncates the expression's float result.
///
/// # Errors
/// The expression compiler's own message, naming what did not parse.
fn eval_disposition(raw: &str) -> Result<Disposition, String> {
    let names: Vec<&str> = Disposition::ALL.iter().map(|&(_, n)| n).collect();
    let vars: Vec<f64> = Disposition::ALL
        .iter()
        .map(|&(f, _)| f64::from(f.bits()))
        .collect();
    let expr =
        vaco_cli_core::value::Expression::compile_with("disposition", raw, &Bindings::new(&names))
            .map_err(|e| e.to_string())?;
    let bits = expr.eval(&vars);
    Ok(Disposition::from_bits_truncate(bits.round() as i64 as u32))
}

/// Parse one `-program` occurrence's `title=string:st=number:...` grammar
/// (measured, `ffmpeg -h full`: the argument placeholder names exactly these
/// two fields plus a repeatable `st`). `id` is left at `0`; the caller
/// numbers programs by declaration order, matching how nothing on this
/// command line states an explicit program id otherwise.
///
/// `st=<n>` names an **output** stream position — the same indexing
/// [`MuxMetadata::stream_tags`] uses — since a program groups the streams
/// this file is actually writing.
///
/// # Errors
/// A message naming the field that did not parse.
fn parse_program(raw: &str) -> Result<vaco_format_core::Program, String> {
    let mut program = vaco_format_core::Program::new(0);
    for field in raw.split(':') {
        if field.is_empty() {
            continue;
        }
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("expected key=value, got '{field}'"))?;
        match key {
            "title" => program
                .metadata
                .push(("title".to_owned(), value.to_owned())),
            "program_num" => {
                program.program_num = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| format!("invalid program_num '{value}'"))?,
                );
            }
            "st" => {
                let idx: u32 = value
                    .parse()
                    .map_err(|_| format!("invalid stream index '{value}'"))?;
                program.stream_indices.push(idx);
            }
            other => return Err(format!("unrecognized program field '{other}'")),
        }
    }
    Ok(program)
}

/// `-metadata key=value` sets; `-metadata key=` (empty value) deletes —
/// measured against the reference, which never keeps a same-key duplicate
/// either way.
fn set_tag(tags: &mut Vec<(String, String)>, key: &str, value: &str) {
    tags.retain(|(k, _)| k != key);
    if !value.is_empty() {
        tags.push((key.to_owned(), value.to_owned()));
    }
}

/// Finish [`ResolvedOutput::metadata`] with `-map_chapters`/`-map_metadata`'s
/// source data (CL-16), which [`metadata_of`] cannot see (it runs before any
/// input is opened).
///
/// Chapters: `out.map_chapters`, or — absent one — the first input that has
/// any, our own reading of the reference's "chapters are copied from the
/// first input file that has any" default; `Some(-1)` (explicit or, for
/// metadata, the computed default when nothing qualifies) copies nothing.
///
/// Global tags: `out.map_metadata`, or input `0` by default (the reference's
/// own stated default); explicit `-metadata` always wins over a copied value
/// for the same key, since it is folded in *after* the copy.
fn resolve_mapped_metadata(
    out: &ResolvedOutput,
    input_chapters: &[Vec<vaco_format_core::Chapter>],
    input_metadata: &[Vec<(String, String)>],
) -> MuxMetadata {
    let mut meta = out.metadata.clone();

    let chapters_from = out
        .map_chapters
        .unwrap_or_else(|| {
            input_chapters
                .iter()
                .position(|c| !c.is_empty())
                .and_then(|i| i64::try_from(i).ok())
                .unwrap_or(-1)
        })
        .max(-1);
    if let Ok(i) = usize::try_from(chapters_from)
        && let Some(chapters) = input_chapters.get(i)
    {
        meta.chapters.clone_from(chapters);
    }

    let metadata_from = out.map_metadata.unwrap_or(0).max(-1);
    if let Ok(i) = usize::try_from(metadata_from)
        && let Some(copied) = input_metadata.get(i)
    {
        let mut tags = copied.clone();
        for (k, v) in &out.metadata.tags {
            set_tag(&mut tags, k, v);
        }
        meta.tags = tags;
    }

    meta
}

/// Which muxer an output resolves to. See the module docs for the three
/// refusals.
///
/// `null` is not special-cased here any more: `vaco_registry::muxer_by_name`
/// finds it the same way it finds `matroska` or `mp4`, because
/// `vaco-mux-utility` registers a real `MUXER_NULL` now. The only remaining
/// special case is in [`run_pipeline`], which still must not touch the
/// filesystem for a `NOFILE` container — that is a property of the
/// *instantiated* muxer, not of its name, so it cannot be decided here.
fn muxer_for(out: &OutputSpec) -> Result<&'static str, Diagnostic> {
    if let Some(name) = out.format.as_deref() {
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

/// Resolve `-c`/`-c:v`/`-c:a` for every output stream: `copy`, a name the
/// registry resolves to a real encoder, or a diagnosis.
///
/// This used to accept only the literal string `"copy"` — every
/// other name was `Unknown encoder '{name}'`, correct behaviour when there
/// were no encoders and wrong the moment one landed. `vaco_registry::
/// encoder_by_name` is what a name is checked against now, so that message is
/// still exactly right, it is just reached only *after* a real lookup fails
/// rather than unconditionally. Measured against the reference (`ffmpeg -c:v
/// mjpegb`, a codec it can decode but not encode): a codec this build merely
/// has no *encoder* for gets the identical "Unknown encoder" message and exit
/// code as a name that names no codec at all — there is no separate "known
/// but unbuilt" message to reproduce.
/// The registered encoder for `format`'s own default codec for `media`, if
/// the container declares one and this build actually has an encoder for it.
///
/// # E2E-GAPS 4: a container's default was declared but never consulted
///
/// `MuxerDesc::default_video`/`default_audio` exist precisely so a stream
/// with no explicit `-c` can still pick something — `vaco-mux-utility`'s
/// `MUXER_NULL` sets `default_video: Some(CodecId::WrappedAvframe)` and
/// `default_audio: Some(CodecId::PcmS16le)` for exactly this reason (see that
/// crate's module doc). [`check_codecs`] used to go straight to the "probably
/// disabled" diagnosis whenever `-c` was absent, without ever asking the
/// resolved muxer what its default was — so `-f null -` refused every run,
/// even though the reference's own null muxer (measured, `ffmpeg 9.0.1`)
/// maps a decodable h264/aac pair to `wrapped_avframe`/`pcm_s16le` and
/// proceeds. A container with no declared default, or one whose declared
/// codec has no registered encoder in this build, still falls through to the
/// original message — this only adds the case the reference itself takes.
fn default_encoder_for(
    format: &str,
    media: Option<MediaType>,
    url: &str,
) -> Option<&'static vaco_codec_core::EncoderDesc> {
    vaco_registry::encoder_for(default_codec_for(format, media, url)?)
}

/// The codec an output with no `-c` gets: the container's declared default,
/// unless it is one of the `image2` family, where the *filename* decides.
///
/// # Why the filename overrides the container here
///
/// `image2` declares exactly one default video codec — `mjpeg`, measured from
/// `ffmpeg -h muxer=image2` — while claiming 42 extensions. Consulting only
/// that field made `vaco -i in.png out.png` select the JPEG encoder and fail
/// with `unsupported: jpeg: only grayscale and ...`, where the reference
/// writes a PNG. The reference resolves this from the output filename before
/// falling back to the declared default, and so does this.
///
/// `vaco_codec_core::image_codec_for_extension` is the single table that
/// answers it; `vaco-mux-image2` and `vaco-demux-image2` read the same one.
fn default_codec_for(format: &str, media: Option<MediaType>, url: &str) -> Option<CodecId> {
    let desc = vaco_registry::muxer_by_name(format)?;
    let media = media?;
    if media == MediaType::Video && desc.matches_name("image2") {
        let guessed = std::path::Path::new(url)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(vaco_codec_core::image_codec_for_extension);
        // A named extension is an instruction, not a hint: if it names a codec
        // this build cannot encode, that is the error to report, not a silent
        // fall back to JPEG.
        if let Some(codec) = guessed {
            return Some(codec);
        }
    }
    desc.default_codec(media)
}

/// The [`vaco_codec_core::CodecParameters`] an **encoded** output stream is
/// described by.
///
/// [`vaco_codec_core::CodecParameters::with_codec`] keeps everything the input declared,
/// which is exactly right for `-c copy` and wrong here. Two of those fields
/// describe the input's *coded form*, and an encoder's output is not it:
///
/// * `extradata` is the input stream's own configuration record, and
///   `with_codec` only drops it when the codec id changes — so re-encoding
///   H.264 to H.264 wrote the *input file's* `avcC` into the output
///   container. Measured: a 1920x1080 source scaled to 1280x600 produced an
///   `avcC` byte-identical to the input's, advertising 1920x1080 and the
///   input's parameter sets over the encoder's own.
/// * `video.nal_length_size` is the input *container's* NAL framing.
///   Inherited `Some(4)` from an MP4 source is what made
///   `-c:v libx264 out.ts` and `out.h264` run `h264_mp4toannexb` over an
///   Annex-B elementary stream: `00 00 00 01` read as a one-byte NAL, and
///   the output decoded as `profile=unknown width=0 height=0`.
///
/// Both are cleared here and repopulated from the encoder itself, which is
/// the only thing that knows.
fn encoded_params(
    input: &vaco_codec_core::CodecParameters,
    id: vaco_codec_core::CodecId,
) -> vaco_codec_core::CodecParameters {
    let mut out = input.clone().with_codec(id);
    out.extradata = None;
    if let Some(v) = out.video.as_mut() {
        v.nal_length_size = None;
    }
    out
}

/// The NAL length prefix width an encoder's packets carry, read off the
/// configuration record that encoder supplied.
///
/// ISO/IEC 14496-15 puts `lengthSizeMinusOne` inside the record, so this
/// derives rather than asserts — `vaco_format_nalu::record_nal_length_size`
/// is the one place that layout is known. No encoder here supplies a record
/// today: they all emit Annex-B elementary streams, which is `None`, which
/// is what the containers that want Annex-B (MPEG-TS, ASF, raw) read to
/// leave the stream alone.
fn declared_nal_length_size(id: vaco_codec_core::CodecId, extradata: Option<&[u8]>) -> Option<u8> {
    let kind = vaco_format_nalu::header_kind_for(id)?;
    vaco_format_nalu::record_nal_length_size(kind, extradata?)
}

fn check_codecs(
    cli: &Cli,
    out: &OutputSpec,
    format: &str,
    streams: &[OutStream],
) -> Result<Vec<StreamCodec>, Diagnostic> {
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

    let mut chosen_codecs = Vec::new();
    for (i, s) in streams.iter().enumerate() {
        let chosen = group
            .and_then(|g| g.stream_option("c", &ctx, i as u32).ok().flatten())
            .and_then(|o| o.value.as_ref())
            .and_then(|v| v.to_str());
        match chosen {
            Some("copy") => chosen_codecs.push(StreamCodec::Copy),
            Some(name) => match vaco_registry::encoder_by_name(name) {
                Some(desc) => chosen_codecs.push(StreamCodec::Encode(desc.name)),
                None => {
                    return Err(encoder_error(
                        out,
                        s,
                        i,
                        &format!("Unknown encoder '{name}'"),
                    ));
                }
            },
            None => match default_encoder_for(format, s.media, &out.url) {
                Some(desc) => chosen_codecs.push(StreamCodec::Encode(desc.name)),
                None => {
                    // The reference names the codec it resolved and could not
                    // encode (`codec jpegxl`), and `none` only when it
                    // resolved nothing at all.
                    let codec = default_codec_for(format, s.media, &out.url)
                        .map_or("none", CodecId::name);
                    return Err(encoder_error(
                        out,
                        s,
                        i,
                        &format!(
                            "Automatic encoder selection failed Default encoder for format {format} (codec {codec}) is probably disabled. Please choose an encoder manually.",
                        ),
                    ));
                }
            },
        }
    }
    Ok(chosen_codecs)
}

/// The `vost`/`aost`/`sost`/`dost` tag the reference prefixes an output
/// stream's own diagnostics with, by media type.
fn stream_tag(media: Option<MediaType>) -> &'static str {
    match media {
        Some(MediaType::Video) => "vost",
        Some(MediaType::Audio) => "aost",
        Some(MediaType::Subtitle) => "sost",
        _ => "dost",
    }
}

fn encoder_error(out: &OutputSpec, s: &OutStream, i: usize, detail: &str) -> Diagnostic {
    let prefix = format!("[{}#{}:{i}]", stream_tag(s.media), out.index);
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

/// CL-22: `-force_key_frames` for every video stream in `streams` — the same
/// [`MatchCtx`] shape [`check_codecs`]/[`graph_options_of`] build, for the
/// same reason (`-force_key_frames:v:0` counts within the output). Parsed and
/// validated here so a malformed value is a real, early diagnostic; not yet
/// acted on — see `crate::force_key_frames`'s module doc for why.
///
/// # Errors
///
/// A [`Diagnostic`] naming what [`force_key_frames::parse`] rejected.
///
/// [`force_key_frames::parse`]: crate::force_key_frames::parse
fn force_key_frames_of(
    cli: &Cli,
    out: &OutputSpec,
    streams: &[OutStream],
) -> Result<Vec<Option<crate::force_key_frames::ForceKeyFrames>>, Diagnostic> {
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
    let Some(group) = cli.output_group(out.index) else {
        return Ok(vec![None; streams.len()]);
    };

    let mut out_opts = Vec::new();
    for (i, s) in streams.iter().enumerate() {
        if s.media != Some(MediaType::Video) {
            out_opts.push(None);
            continue;
        }
        let idx = i as u32;
        let resolved = match group.stream_option("force_key_frames", &ctx, idx) {
            Ok(Some(opt)) => {
                let raw = value_str(opt)?;
                Some(crate::force_key_frames::parse(&raw).map_err(|e| {
                    Diagnostic::new(
                        AvError::EINVAL,
                        vec![format!(
                            "Error parsing option 'force_key_frames' with value '{raw}': {e}"
                        )],
                    )
                })?)
            }
            _ => None,
        };
        out_opts.push(resolved);
    }
    Ok(out_opts)
}

/// The generic `-b`/`-qscale` encoder options (and their aliases: `-b:v`,
/// `-vb`, `-ab`, `-q`, `-qscale`, `-aq`) for every stream in `streams` —
/// resolved the same way [`force_key_frames_of`] resolves its own option,
/// since the underlying grammar is identical (a per-stream value, last match
/// wins). [`run_pipeline`] feeds the result to
/// [`vaco_codec_core::Encoder::set_option`] once the stream's encoder is
/// built, which is the CLI-to-encoder-option path `vaco_codec_core::Encoder`
/// did not use to have — see that method's own doc.
///
/// Both options alias down to two stored names regardless of spelling:
/// `"b"` (`-b`/`-b:v`/`-vb`/`-ab`) and `"q"` (`-q`/`-qscale`/`-aq`). `"b"` is
/// an [`vaco_cli_core::value::ValueKind::Expr`] (`-b:v 2*1000` is valid) and
/// evaluated with [`vaco_cli_core::value::eval_once`]; `"q"` is a plain
/// [`vaco_cli_core::value::ValueKind::Float`] and read with
/// [`vaco_cli_core::value::parse_number`]. The stored option key sent to
/// [`vaco_codec_core::Encoder::set_option`] is `"b"` or `"qscale"` — the
/// latter renamed from `"q"` because that is the name the reference's own
/// `AVOption` table uses for the same field, and the one an `Encoder`
/// implementation should recognise.
///
/// # Errors
/// A [`Diagnostic`] naming what `eval_once`/`parse_number` rejected.
fn codec_options_of(
    cli: &Cli,
    out: &OutputSpec,
    streams: &[OutStream],
) -> Result<Vec<Vec<(String, String)>>, Diagnostic> {
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
    let Some(group) = cli.output_group(out.index) else {
        return Ok(vec![Vec::new(); streams.len()]);
    };

    let mut out_opts = Vec::new();
    for i in 0..streams.len() {
        let idx = i as u32;
        let mut opts = Vec::new();
        if let Ok(Some(opt)) = group.stream_option("b", &ctx, idx) {
            let raw = value_str(opt)?;
            let bps = vaco_cli_core::value::eval_once("b", &raw).map_err(|e| {
                Diagnostic::new(
                    AvError::EINVAL,
                    vec![format!("Error parsing option 'b' with value '{raw}': {e}")],
                )
            })?;
            opts.push(("b".to_owned(), format!("{}", bps as i64)));
        }
        if let Ok(Some(opt)) = group.stream_option("q", &ctx, idx) {
            let raw = value_str(opt)?;
            let q = vaco_cli_core::value::parse_number(
                "q",
                &raw,
                vaco_cli_core::value::NumberLimits::float(),
            )
            .map_err(|e| {
                Diagnostic::new(
                    AvError::EINVAL,
                    vec![format!("Error parsing option 'q' with value '{raw}': {e}")],
                )
            })?;
            opts.push(("qscale".to_owned(), q.to_string()));
        }
        // Codec-private options this build's own encoders actually
        // implement (vaco-codec-png's `pred`/`compression_level`,
        // vaco-codec-tiff's `compression_algo`, vaco-codec-exr's
        // `compression`, vaco-codec-ffv1's `coder`/`slices`), passed through
        // as opaque strings -- unlike `-b`/`-q` above, none of these has its
        // own CLI-side grammar to evaluate. Real ffmpeg resolves a name like
        // this by searching every codec's `AVOption` class at split time;
        // `crate::cli::Oracle::knows` is this build's hand-maintained mirror
        // of that search for exactly this list, so a name has to be added in
        // both places or the option never reaches the split stage at all.
        for name in PRIVATE_ENCODER_OPTIONS {
            if let Ok(Some(opt)) = group.stream_option(name, &ctx, idx) {
                opts.push(((*name).to_owned(), value_str(opt)?));
            }
        }
        out_opts.push(opts);
    }
    Ok(out_opts)
}

/// Every codec-private `Encoder::set_option` key this build's own encoders
/// recognise, shared between [`codec_options_of`] (which resolves a value
/// for each) and [`crate::cli::Oracle::knows`] (which is what lets one past
/// the split stage at all — see that function's own doc for why there is no
/// registry-wide reflection to consult instead).
pub(crate) const PRIVATE_ENCODER_OPTIONS: &[&str] = &[
    "pred",
    "compression_level",
    "compression_algo",
    "compression",
    "coder",
    "slices",
];

/// `-bsf`/`-bsf:v`/`-bsf:a`/`-bsf:s` for every stream in `streams` — the
/// bitstream-filter reachability sweep's systemic finding closed: 21
/// registered, working, tested filters existed with no way for any user to
/// invoke them (`-bsf` itself was already in the option table, per
/// `vaco-cli-core/src/tables/ffmpeg.rs`, and already parsed with full
/// stream-specifier support by [`OptionGroup::stream_option`] — the same
/// machinery [`codec_options_of`]'s `-b`/`-q` use above — but nothing ever
/// called it).
///
/// Each stream's value is a comma-separated chain,
/// `name[=opt=val[:opt=val...]]` per entry — the reference's own grammar
/// (`ffmpeg -h bsf=<name>`'s own examples read this way, e.g.
/// `h264_metadata=aud=insert`). A name is resolved against the registry
/// immediately, here, rather than deferred to mux time: `-bsf:v
/// nosuchfilter` gets refused before a single byte is written, the same
/// "Bitstream filter not found" shape the reference reports at option-parse
/// time (measured, `ffmpeg 9.0.1`), not a runtime surprise deep inside a
/// pipeline that has already opened its output file.
///
/// # Per-instance options are refused, not silently dropped
///
/// [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
/// string (`planning/INTERFACE-GAPS.md` gap 12) — the same gap that keeps
/// every `*_metadata` bitstream filter an identity transform today (see the
/// bsf reachability sweep's own findings). A chain entry that names one is
/// refused here, by name, rather than opened bare as if the option had never
/// been given: silently ignoring `-bsf:v h264_metadata=aud=insert` would
/// write a file the option's own presence promised would have AUD NAL units
/// inserted and does not, which is a wrong-output bug wearing a
/// not-yet-implemented costume. Closing gap 12 itself — giving
/// `BsfProvider::open` an option string every `vaco-bsf-*` crate's `build`
/// can read — is a separate, non-trivial change to that trait and every one
/// of its eight implementors; out of scope here.
///
/// # Errors
///
/// A [`Diagnostic`] for an unknown filter name, a malformed `opt=val`
/// entry, a filter given options this build cannot pass through yet, or
/// non-UTF-8 option text.
fn bsf_options_of(
    cli: &Cli,
    out: &OutputSpec,
    streams: &[OutStream],
) -> Result<Vec<Vec<vaco_format_core::mux::UserBsf>>, Diagnostic> {
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
    let Some(group) = cli.output_group(out.index) else {
        return Ok(vec![Vec::new(); streams.len()]);
    };

    let mut out_chains = Vec::new();
    for i in 0..streams.len() {
        let idx = i as u32;
        let chain = if let Ok(Some(opt)) = group.stream_option("bsf", &ctx, idx) {
            let raw = value_str(opt)?;
            parse_bsf_chain(&raw)?
        } else {
            Vec::new()
        };
        out_chains.push(chain);
    }
    Ok(out_chains)
}

/// One `-bsf`/`-bsf:v`/`-bsf:a`/`-bsf:s` value: `name[=opt=val[:opt=val...]]`,
/// comma-separated between chain entries. See [`bsf_options_of`]'s own doc
/// for the grammar's source and for why a non-empty option list is refused
/// rather than dropped.
///
/// # Errors
/// A [`Diagnostic`] for an unknown filter name, a malformed `opt=val` entry,
/// or any options at all (gap 12).
fn parse_bsf_chain(raw: &str) -> Result<Vec<vaco_format_core::mux::UserBsf>, Diagnostic> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        if entry.is_empty() {
            continue;
        }
        let (name, opts_str) = entry
            .split_once('=')
            .map_or((entry, None), |(n, o)| (n, Some(o)));
        let canonical = vaco_registry::bsf_canonical_name(name).ok_or_else(|| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Error parsing bitstream filter sequence '{raw}': Bitstream filter '{name}' not found"
                )],
            )
        })?;
        let mut options = Vec::new();
        if let Some(opts_str) = opts_str {
            for kv in opts_str.split(':') {
                let Some((k, v)) = kv.split_once('=') else {
                    return Err(Diagnostic::new(
                        AvError::EINVAL,
                        vec![format!(
                            "Error parsing bitstream filter sequence '{raw}': bitstream filter '{name}': malformed option '{kv}' (want key=value)"
                        )],
                    ));
                };
                options.push((k.to_owned(), v.to_owned()));
            }
        }
        if !options.is_empty() {
            return Err(Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Error parsing bitstream filter sequence '{raw}': bitstream filter '{name}' was given options, but this build cannot pass per-instance options to a bitstream filter yet -- use '{name}' with no options"
                )],
            ));
        }
        out.push(vaco_format_core::mux::UserBsf {
            name: canonical,
            options,
        });
    }
    Ok(out)
}

/// CL-20: `-vf`/`-af`/`-filter`, `-s`, `-aspect`, `-pix_fmt` for every stream
/// in `streams` — resolved against the output's own stream list, the same
/// [`MatchCtx`] shape [`check_codecs`] and [`metadata_of`] already build for
/// the same reason (a specifier like `-pix_fmt:v:0` counts within the output,
/// not the input).
///
/// Must run after `streams`' `codec` field is decided: `-vf`/`-af`/`-filter`
/// on a stream resolved to [`StreamCodec::Copy`] is the reference's own
/// "Filtering and streamcopy cannot be used together" error (measured,
/// `ffmpeg 8.1`, exit 234) — but `-s`/`-aspect`/`-pix_fmt` alone on a copied
/// stream are silently ignored (also measured: `-c copy -s 32x32` exits 0
/// with the stream's original geometry unchanged), so only `filter_text`
/// triggers the conflict here.
///
/// # Errors
///
/// A [`Diagnostic`] for the streamcopy/filtergraph conflict above, an invalid
/// `-s` value, or non-UTF-8 option text.
fn graph_options_of(
    cli: &Cli,
    out: &OutputSpec,
    streams: &[OutStream],
    files: &[select::InputStreams],
) -> Result<Vec<crate::filtergraph::SimpleGraphOptions>, Diagnostic> {
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
    // `-auto_conversion_filters` is `bit::GLOBAL`, not per-stream, so this is
    // decided once for the whole output.
    let auto_conversion = !cli
        .line
        .last_global("auto_conversion_filters")
        .is_some_and(|o| o.negated);

    let mut out_opts = Vec::new();
    for (i, s) in streams.iter().enumerate() {
        let mut opts = crate::filtergraph::SimpleGraphOptions {
            auto_conversion,
            ..crate::filtergraph::SimpleGraphOptions::default()
        };
        if let Some(g) = group {
            let idx = i as u32;
            if let Ok(Some(o)) = g.stream_option("filter", &ctx, idx) {
                opts.filter_text = Some(value_str(o)?);
            }
            if s.media == Some(MediaType::Video) {
                if let Ok(Some(o)) = g.stream_option("s", &ctx, idx) {
                    let raw = value_str(o)?;
                    let (w, h) = vaco_core::parse::image_size(&raw).ok_or_else(|| {
                        Diagnostic::new(AvError::EINVAL, vec![format!("Invalid size '{raw}'")])
                    })?;
                    opts.size = Some((w, h));
                }
                if let Ok(Some(o)) = g.stream_option("aspect", &ctx, idx) {
                    opts.aspect = Some(value_str(o)?);
                }
                if let Ok(Some(o)) = g.stream_option("pix_fmt", &ctx, idx) {
                    opts.pix_fmt = Some(value_str(o)?);
                }
                // `-fps_mode` carries `bit::VIDEO` in the option
                // table (`vaco-cli-core/src/tables/ffmpeg.rs`), matching the
                // reference's own video-only scope.
                if let Ok(Some(o)) = g.stream_option("fps_mode", &ctx, idx) {
                    let raw = value_str(o)?;
                    opts.fps_mode = Some(
                        crate::fps_mode::FpsMode::parse(&raw)
                            .map_err(|e| Diagnostic::new(AvError::EINVAL, vec![e]))?,
                    );
                }
                opts.rotate = resolve_rotate(cli, files, s.source)?;
            }
            // `-enc_time_base` is not `bit::VIDEO`-gated in the
            // table (audio encoders take a time base too), so this is
            // resolved for every media type, not just inside the video arm
            // above.
            if let Ok(Some(o)) = g.stream_option("enc_time_base", &ctx, idx) {
                let raw = value_str(o)?;
                opts.enc_time_base = Some(
                    crate::enc_time_base::EncTimeBase::parse(&raw)
                        .map_err(|e| Diagnostic::new(AvError::EINVAL, vec![e]))?,
                );
            }
            if s.media == Some(MediaType::Audio) {
                if let Ok(Some(o)) = g.stream_option("ar", &ctx, idx) {
                    let raw = value_str(o)?;
                    let hz = vaco_cli_core::value::parse_number(
                        "ar",
                        &raw,
                        vaco_cli_core::value::NumberLimits::int32(),
                    )
                    .map_err(|e| {
                        Diagnostic::new(
                            AvError::EINVAL,
                            vec![format!("Error parsing option 'ar' with value '{raw}': {e}")],
                        )
                    })?;
                    if hz > 0.0 {
                        opts.sample_rate = Some(hz as u32);
                    }
                }
                if let Ok(Some(o)) = g.stream_option("ac", &ctx, idx) {
                    let raw = value_str(o)?;
                    let channels = vaco_cli_core::value::parse_number(
                        "ac",
                        &raw,
                        vaco_cli_core::value::NumberLimits::int32(),
                    )
                    .map_err(|e| {
                        Diagnostic::new(
                            AvError::EINVAL,
                            vec![format!("Error parsing option 'ac' with value '{raw}': {e}")],
                        )
                    })?;
                    if channels > 0.0 {
                        opts.channels = Some(channels as u32);
                    }
                }
                if let Ok(Some(o)) = g.stream_option("sample_fmt", &ctx, idx) {
                    opts.sample_fmt = Some(value_str(o)?);
                }
            }
        }
        if opts.filter_text.is_some() && matches!(s.codec, StreamCodec::Copy) {
            return Err(filter_streamcopy_conflict(
                out,
                s,
                i,
                opts.filter_text.as_deref().unwrap_or_default(),
            ));
        }
        out_opts.push(opts);
    }
    Ok(out_opts)
}

/// Measured (`ffmpeg 8.1`, `-map '[out]' -c copy`): streamcopy requested for
/// an output stream fed by a complex filtergraph is a hard error, exit 234:
///
/// ```text
/// [vost#0:0] Streamcopy requested for output stream fed from a complex filtergraph. Filtering and streamcopy cannot be used together.
/// Error opening output file out.mp4.
/// Error opening output files: Invalid argument
/// ```
fn complex_streamcopy_conflict(out: &OutputSpec, s: &OutStream, i: usize) -> Diagnostic {
    let prefix = format!("[{}#{}:{i}]", stream_tag(s.media), out.index);
    Diagnostic::opening(
        AvError::EINVAL,
        vec![format!(
            "{prefix} Streamcopy requested for output stream fed from a complex filtergraph. \
             Filtering and streamcopy cannot be used together."
        )],
        "output",
        &out.url,
    )
}

/// Measured (`ffmpeg 8.1`, `-c copy -vf hflip`): a filtergraph named on a
/// stream resolved to streamcopy is a hard error, exit 234, sans pointer:
///
/// ```text
/// [vost#0:0/copy] Filtergraph 'hflip' was specified, but codec copy was selected. Filtering and streamcopy cannot be used together.
/// Error opening output file -.
/// Error opening output files: Invalid argument
/// ```
fn filter_streamcopy_conflict(out: &OutputSpec, s: &OutStream, i: usize, text: &str) -> Diagnostic {
    let prefix = format!("[{}#{}:{i}/copy]", stream_tag(s.media), out.index);
    Diagnostic::opening(
        AvError::EINVAL,
        vec![format!(
            "{prefix} Filtergraph '{text}' was specified, but codec copy was selected. \
             Filtering and streamcopy cannot be used together."
        )],
        "output",
        &out.url,
    )
}

/// Build and run the pipeline.
///
/// Consumes the inputs, because `vaco-sched` takes each demuxer by value.
///
/// `complex_filters` is `cli.complex_filters` (CL-25) — every
/// `-filter_complex`/`-lavfi` occurrence's text, rebuilt here for real (a
/// second parse of the same text [`crate::complexgraph::catalog`] already
/// parsed once, structurally, to resolve `-map [label]` before any input was
/// opened — see that module's docs for why building it twice is safe rather
/// than a duplicated risk). `auto_conversion_filters` is
/// `-auto_conversion_filters`'s global default, applied to every complex
/// graph the same way [`graph_options_of`] already applies it per output
/// stream to a simple one.
///
/// # Errors
///
/// A [`Diagnostic`] for a wiring failure the builder rejects, or for a pipeline
/// that stalls or is cancelled.
pub fn run_pipeline(
    inputs: Vec<InputFile>,
    outputs: &[ResolvedOutput],
    files: &[InputStreams],
    complex_filters: &[String],
    auto_conversion_filters: bool,
    threads: usize,
    filter_threads: usize,
    overwrite: crate::overwrite::OverwritePolicy,
) -> Result<RunSpec, Diagnostic> {
    if outputs.iter().all(|o| o.dropped) {
        // Nothing to write anywhere, so there is nothing to read either. The
        // reference does not open the output file at all in this case; reading
        // the inputs to end of stream and discarding every packet would be a
        // slow way of doing the same nothing.
        return Ok(RunSpec::default());
    }

    // The time base rides alongside the params rather than in them —
    // `CodecParameters` has no such field, `vaco_format_core::Stream` does —
    // and a transcoded stream needs its input's time base: an
    // encoder's `add_encoder` takes one explicitly, and there is no other
    // demuxer left to ask once `f.demuxer` moves into `spec` below.
    let params: Vec<Vec<(u32, vaco_codec_core::CodecParameters, vaco_core::Rational)>> = inputs
        .iter()
        .map(|f| {
            f.demuxer
                .streams()
                .iter()
                .map(|s| (s.index, s.params.clone(), s.time_base))
                .collect()
        })
        .collect();

    // CL-16, `-map_chapters`/`-map_metadata`'s source data: read here, from
    // `&inputs`, because the loop below moves each `f.demuxer` into `spec`
    // one at a time and a `Demuxer` gives up `chapters()`/`metadata()` for
    // good the moment that happens.
    let input_chapters: Vec<Vec<vaco_format_core::Chapter>> = inputs
        .iter()
        .map(|f| f.demuxer.chapters().to_vec())
        .collect();
    let input_metadata: Vec<Vec<(String, String)>> = inputs
        .iter()
        .map(|f| f.demuxer.metadata().to_vec())
        .collect();
    // `-stats`'s `time=` approximation (`crate::stats`): the longest stated
    // input duration, read here for the same reason as the two collections
    // above — after this function's own consuming loop, no `Demuxer` is
    // reachable at all.
    let input_duration_secs = inputs
        .iter()
        .filter_map(|f| f.demuxer.duration())
        .map(|d| d.as_micros() as f64 / 1_000_000.0)
        .fold(None, |acc: Option<f64>, d| {
            Some(acc.map_or(d, |a| a.max(d)))
        });

    let mut spec = PipelineSpec::new();
    let mut refs = Vec::new();
    for f in inputs {
        refs.push(spec.add_input(f.demuxer));
    }

    // CL-25: build every `-filter_complex`/`-lavfi` graph for real, once,
    // before any output stream is wired. `complex_taps[i]` corresponds to
    // `crate::complexgraph::catalog`'s `i`-th labelled pad — both walk
    // `complex_filters` in the same order and keep only `label.is_some()`
    // outputs, so the index spaces agree without either side inspecting the
    // other (see that module's docs).
    let mut complex_used_inputs: HashSet<(u32, u32)> = HashSet::new();
    let mut complex_taps: Vec<(
        vaco_sched::FrameTap,
        vaco_core::Rational,
        vaco_codec_core::CodecParameters,
    )> = Vec::new();
    for text in complex_filters {
        let built = complexgraph::build_and_attach(
            &mut spec,
            text,
            files,
            &refs,
            &params,
            &mut complex_used_inputs,
            auto_conversion_filters,
        )
        .map_err(|e| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!("Error configuring filter graph: {e}")],
            )
        })?;
        complex_taps.extend(
            built
                .into_iter()
                .filter(|o| o.label.is_some())
                .map(|o| (o.tap, o.time_base, o.params)),
        );
    }

    let mut report = RunSpec::default();
    // Per output, in the same order as `outputs.iter().filter(..)`: the packet
    // tally and, unless the container is `NOFILE`, a handle to the real bytes
    // written — `open_output`'s docs say why the two are not the same thing.
    let mut sinks: Vec<(Sink, Option<Arc<AtomicU64>>)> = Vec::new();
    for out in outputs.iter().filter(|o| !o.dropped) {
        let (inner, high_water) = open_output(out, overwrite)?;
        let muxer: Box<dyn Muxer> = Box::new(TallyingMuxer::new(inner, out.sink.clone()));

        // `add_output_with`, not the plain `add_output`: the container's
        // flags still come from the muxer itself — `MuxBuilder::new` reads
        // them once, inside `spec.add_output_with`, and a caller preference
        // cannot override `TS_NONSTRICT`/`NOTIMESTAMPS` — but `options` is
        // FW-11's output-side share of the generic `AVFormatContext` table
        // (`-avoid_negative_ts`, `-max_interleave_delta`, …) instead of
        // always `default()`.
        let oref = spec.add_output_with(muxer, &out.format_opts);

        // CL-16 (gap 1): resolve `-map_chapters`/`-map_metadata` against the
        // inputs actually opened, then attach it to the output through
        // `PipelineSpec::set_output_metadata`. `PipelineSpec::build` delivers
        // it to `Muxer::set_metadata` at `MuxBuilder::open` (M30) — after
        // every stream is declared and its time base is settled, but before
        // the header. This used to call `Muxer::set_metadata` on the muxer
        // directly, before a single stream existed, because `vaco-sched`'s
        // `MuxWork` drove a raw `dyn Muxer` with no later point at which this
        // module could still reach it. `MuxWork` now drives a `MuxWriter`,
        // and `MuxBuilder` is the later point. `vaco-mux-mp4`/`vaco-mux-matroska`/
        // `vaco-mux-stream`'s `ffmetadata` still resolve per-stream fields
        // lazily at `write_header` time; that laziness is no longer required
        // by this ordering, but changing those crates is out of this one's
        // scope.
        let meta = resolve_mapped_metadata(out, &input_chapters, &input_metadata);
        spec.set_output_metadata(oref, meta)
            .map_err(|e| internal_from("the muxer refused metadata", &e))?;

        // M6: the bitstream filters a muxer's `check_bitstream` asks for by
        // name. Without this the stage runs against `NoBsfs` and errs on the
        // first request, which is not a corner case — it is `-c copy` from an
        // MP4 to anything Annex-B:
        //
        //     $ vaco -i in.mp4 -c copy -f mpegts out.ts
        //     Error while filtering: unsupported: this muxer needs a bitstream
        //     filter and no BsfProvider was supplied
        //
        // `vaco_registry::Bsfs` has existed since gap 8 closed and had no
        // caller; `PipelineSpec::set_output_bsfs` had none either. Both halves
        // were built and neither was connected, so MP4 -> TS and MP4 -> AVI
        // remux failed outright while every unit test passed.
        spec.set_output_bsfs(oref, Arc::new(vaco_registry::Bsfs))
            .map_err(|e| internal_from("the muxer refused a bitstream filter", &e))?;
        sinks.push((out.sink.clone(), high_water));
        for (i, s) in out.streams.iter().enumerate() {
            let (packet_tap, out_params, label, mapping_source) = match &s.source {
                StreamPick::Demuxed { file, stream } => {
                    let Some(input) = refs.get(*file as usize).copied() else {
                        return Err(internal("a map names an input that was not opened"));
                    };
                    let tap = spec
                        .input_stream(input, *stream)
                        .map_err(|_| internal("a map names a stream the demuxer does not have"))?;
                    let (p, time_base) = params
                        .get(*file as usize)
                        .and_then(|v| v.iter().find(|(idx, _, _)| *idx == *stream))
                        .map_or_else(
                            || {
                                (
                                    vaco_codec_core::CodecParameters::new(MediaType::Data),
                                    vaco_core::Rational::new(1, 1),
                                )
                            },
                            |(_, p, tb)| (p.clone(), *tb),
                        );

                    // A stream this crate copies has no decoder or encoder
                    // in the graph at all — `spec.map` on the demuxer's own tap is
                    // what makes it a copy. A stream with a resolved encoder gets a
                    // decode leg and an encode leg in between, per `vaco-sched`'s own
                    // `add_decoder`/`add_encoder` (already exercised by its mocks;
                    // nothing there needed to change for this).
                    let (packet_tap, out_params, label) = match s.codec {
                        StreamCodec::Copy => (tap, p, "copy".to_owned()),
                        StreamCodec::Encode(name) => {
                            let codec_id = p.codec_id.ok_or_else(|| {
                                internal("a stream being transcoded has no known input codec")
                            })?;
                            let decoder_desc =
                                vaco_registry::decoder_for(codec_id).ok_or_else(|| {
                                    internal("this build has no decoder for the input codec")
                                })?;
                            let encoder_desc =
                                vaco_registry::encoder_by_name(name).ok_or_else(|| {
                                    internal("the resolved encoder is no longer in the registry")
                                })?;
                            // `Limits::default()` is `strict()` (`vaco-limits`'s
                            // deliberate "nobody thought about it" fallback for
                            // an embedder on untrusted input) — wrong for the
                            // CLI, whose whole job is decoding a file the user
                            // handed it directly. `docs/core/vaco-limits.md`
                            // names `permissive()` the CLI default explicitly.
                            let limits = vaco_limits::Limits::permissive();
                            let mut decoder = decoder_desc.build(limits.clone());
                            // `-threads N`. `Decoder::set_thread_count`'s
                            // default is a no-op reporting `Threading::None`,
                            // so every decoder that has not implemented
                            // frame threading is untouched by this, and a
                            // run that did not say `-threads` passes 1 and
                            // takes the same path it always did.
                            let _granted = decoder.set_thread_count(threads);
                            if let Some(v) = p.video.as_ref() {
                                // `Decoder::prime_video`'s own default is a
                                // no-op, so this costs every decoder except
                                // the one that actually needs it (FFV1)
                                // nothing. Coded size, not display size —
                                // RFC 9043's
                                // `frame_pixel_width`/`frame_pixel_height`
                                // describe the decoded picture, matching the
                                // "coded" half of the display/coded split
                                // this build already carries per-codec,
                                // measured directly rather than assumed.
                                let (width, height) = if v.coded_width > 0 && v.coded_height > 0 {
                                    (v.coded_width, v.coded_height)
                                } else {
                                    (v.width, v.height)
                                };
                                if width > 0 && height > 0 {
                                    decoder.prime_video(width, height);
                                }
                            }
                            if let Some(a) = p.audio.as_ref() {
                                // `Decoder::prime_audio`'s own default is a
                                // no-op, so this costs every decoder whose
                                // bitstream states its own rate/layout
                                // (nearly all of them) nothing. It is what
                                // `vaco-codec-pcm` needs: raw PCM's bitstream
                                // carries neither, and the container's own
                                // declared parameters are the only source of
                                // truth for them — the audio mirror of
                                // `prime_video`'s FFV1 case just above.
                                decoder.prime_audio(
                                    a.sample_rate,
                                    a.layout
                                        .clone()
                                        .unwrap_or(vaco_chlayout::ChannelLayout::unspecified(0)),
                                );
                            }
                            if let Some(extradata) = p.extradata.as_deref() {
                                // Offering, not requiring: `Decoder::set_extradata`'s
                                // own contract says a caller offering a container's
                                // configuration record should treat a refusal as
                                // "this decoder had nothing to learn from it", the
                                // same convention `Parser::set_extradata` already
                                // uses, so a decoder with no use for the record (or
                                // no override at all) just keeps decoding.
                                let _ = decoder.set_extradata(extradata);
                            }
                            let frames = spec
                                .add_decoder(tap, decoder)
                                .map_err(|e| internal_from("could not attach a decoder", &e))?;
                            let mut encoder = encoder_desc.build(limits.clone());
                            // CL-33/central gap: `-b`/`-qscale` (any spelling),
                            // resolved by `codec_options_of`. A codec with no
                            // use for a given key ignores it per
                            // `Encoder::set_option`'s own default; a value it
                            // does recognise but cannot parse is a real error.
                            for (key, value) in &s.codec_options {
                                encoder.set_option(key, value).map_err(|e| {
                                    internal_from("the encoder refused an option", &e)
                                })?;
                            }
                            // An encoder that does not care lists nothing, so this is
                            // a no-op for the common case.
                            let accepted = encoder.accepted_pix_fmts();
                            // The audio twin, needed before the graph/non-graph
                            // branch below rather than only inside the
                            // non-graph half: the graph path feeds it to
                            // `crate::filtergraph::build` as the audio sink's
                            // own constraint, the same role `accepted` plays
                            // for the video sink.
                            let accepted_audio = encoder.accepted_sample_fmts();
                            // What the stream actually carries once this leg's
                            // conversions (if any) are wired in — filled in
                            // below whenever a converter changes the format
                            // the encoder is fed, and applied to `out_params`
                            // after this block. Without this, `out_params`
                            // kept reporting the *source's* format after a
                            // transcode: a real `-c:a pcm_s16le` from a planar
                            // decoder (`alac`, `flac`) produced correctly
                            // packed PCM bytes but still described the stream
                            // as planar to the muxer, and `wav`'s own strict
                            // check refused it — "wav: planar sample formats
                            // are not supported" for a stream whose bytes
                            // were never planar (E2E-GAPS 3's own reported
                            // symptom, reached from the `alac`/`wav` pair
                            // once the encoder-side mismatch above was
                            // fixed).
                            let mut out_video_format: Option<PixFmt> = None;
                            // The graph's real output geometry, for the same
                            // reason `out_video_format` exists: a
                            // `-vf scale=...`/`-s` stream's frames are not the
                            // size the input declared. Without this, `tkhd` on
                            // a 1920x1080 source cropped to 1280x600 said
                            // 1920x1080 while every sample was 1280x600 —
                            // measured against ffmpeg 9.0.1, whose own `tkhd`
                            // says 1280x600.
                            let mut out_video_size: Option<(u32, u32)> = None;
                            let mut out_audio_format: Option<vaco_sampfmt::SampleFmt> = None;
                            let mut out_audio_rate: Option<u32> = None;
                            let mut out_audio_layout: Option<vaco_chlayout::ChannelLayout> = None;
                            let frames = if s.graph_opts.wants_graph() {
                                // CL-20: a real `-vf`/`-af`/`-filter`/`-s`/`-aspect`/
                                // `-pix_fmt`/`-ar`/`-ac`/`-sample_fmt` graph replaces
                                // the ad-hoc `converter_target`/`add_converter` path
                                // below — `SimpleGraph::build`'s own `configure`
                                // already runs the same auto-conversion policy for
                                // whatever the user's chain leaves unresolved
                                // against `accepted`/`accepted_audio`.
                                let built = crate::filtergraph::build(
                                    &s.graph_opts,
                                    s.media.unwrap_or(MediaType::Data),
                                    p.video.as_ref(),
                                    p.audio.as_ref(),
                                    time_base,
                                    accepted,
                                    accepted_audio,
                                )
                                .map_err(|e| {
                                    Diagnostic::new(AvError::EINVAL, vec![format!("Error: {e}")])
                                })?;
                                // What the sink actually resolved to, so
                                // `out_params` below describes the graph's real
                                // output rather than the pre-graph decode
                                // parameters — the same gap E2E-GAPS 3 closed for
                                // the non-graph path's sample *format*, but this
                                // block had never been reached for a filtered
                                // stream at all: `out_video_format`/
                                // `out_audio_format` stayed `None` and the muxer
                                // kept whatever `p.with_codec` copied over, wrong
                                // for a `-s`/`-ar`/`-af aresample=...` stream in
                                // exactly the way a `WAV` header claiming the
                                // input's sample rate while the samples were
                                // resampled to a different one would be wrong.
                                match built.graph.sink_format(built.sink) {
                                    Ok(LinkFormat::Video {
                                        format,
                                        width,
                                        height,
                                        ..
                                    }) => {
                                        out_video_format = Some(*format);
                                        out_video_size = Some((*width, *height));
                                    }
                                    Ok(LinkFormat::Audio {
                                        format,
                                        sample_rate,
                                        layout,
                                        ..
                                    }) => {
                                        out_audio_format = Some(*format);
                                        out_audio_rate = Some(*sample_rate);
                                        out_audio_layout = Some(layout.clone());
                                        // Same reason as the non-graph branch's
                                        // own call below: an encoder whose
                                        // packets are not self-describing needs
                                        // the real stream shape before the first
                                        // frame, and this is the first point the
                                        // graph's negotiated shape is known.
                                        encoder.prime_audio(*sample_rate, layout.clone(), *format);
                                    }
                                    Err(_) => {}
                                }
                                spec.add_filter(
                                    built.graph,
                                    &[SourceBind::new(frames, built.source, time_base)],
                                    &[built.sink],
                                )
                                .map_err(|e| internal_from("could not attach a filtergraph", &e))?
                                .into_iter()
                                .next()
                                .ok_or_else(|| {
                                    internal("a configured filtergraph produced no sink tap")
                                })?
                            } else {
                                let source_format = p.video.as_ref().and_then(|v| v.format);
                                let target = converter_target(source_format, accepted);
                                let frames = match target {
                                    Some(t) if Some(t) != source_format => {
                                        out_video_format = Some(t);
                                        spec.add_converter(
                                            frames,
                                            t,
                                            time_base,
                                            limits.clone(),
                                            i32::try_from(filter_threads).unwrap_or(i32::MAX),
                                        )
                                        .map_err(|e| {
                                            internal_from("could not attach a format converter", &e)
                                        })?
                                    }
                                    _ => frames,
                                };
                                // E2E-GAPS 3: the audio twin of the pixel-format
                                // conversion just above — with one real
                                // difference from the video case, which is why
                                // this does not reuse `converter_target`'s
                                // "skip the node if the source already looks
                                // right" shape.
                                //
                                // `p.video.format` is a fact the SPS/VPS states
                                // and a decoder is required to honour exactly,
                                // so comparing it against `accepted` ahead of
                                // decoding is reliable. `p.audio.format` is not
                                // the same kind of fact: it is the
                                // *container's* best guess before a single
                                // sample has been decoded (measured: a
                                // Matroska FLAC track reports `s16`, packed,
                                // while `vaco-codec-flac`'s real decoder
                                // output is `s16p`, planar — there is no
                                // Matroska field that could have told the
                                // demuxer that). Gating insertion on that
                                // guess reproduces the exact bug this closes:
                                // the guess happens to already be in
                                // `accepted`, no converter gets wired, and the
                                // real (different) runtime format reaches the
                                // encoder unconverted.
                                //
                                // So: whenever the encoder names a specific
                                // accepted list at all, the converter is
                                // always wired, unconditionally.
                                // `AudioConverterSide::convert` already does
                                // the cheap per-frame passthrough when the
                                // real format turns out to already match
                                // (`vaco-sched`'s own test coverage), so this
                                // costs nothing in the common case and fixes
                                // the mismatched one.
                                match accepted_audio.first() {
                                    Some(&t) => {
                                        out_audio_format = Some(t);
                                        // Give the encoder the stream shape
                                        // it is about to
                                        // receive *before* the first frame,
                                        // so an encoder whose packets are
                                        // not self-describing (`flac`'s
                                        // defer sample rate to `STREAMINFO`)
                                        // can answer `Encoder::extradata`
                                        // in time for `out_params` below —
                                        // `Muxer::add_stream` happens once,
                                        // before this pipeline runs a single
                                        // frame through it, so "wait for the
                                        // first frame" (this trait method's
                                        // own default, and every encoder's
                                        // behaviour before it existed) is
                                        // always too late.
                                        let sample_rate =
                                            p.audio.as_ref().map_or(0, |a| a.sample_rate);
                                        let layout = p
                                            .audio
                                            .as_ref()
                                            .and_then(|a| a.layout.clone())
                                            .unwrap_or(vaco_chlayout::ChannelLayout::MONO);
                                        encoder.prime_audio(sample_rate, layout, t);
                                        spec.add_sample_converter(
                                            frames,
                                            t,
                                            time_base,
                                            limits.clone(),
                                        )
                                        .map_err(|e| {
                                            internal_from(
                                                "could not attach a sample-format converter",
                                                &e,
                                            )
                                        })?
                                    }
                                    None => frames,
                                }
                            };
                            // `-fps_mode` (video-only), inserted
                            // after any `-vf`/auto-conversion and immediately
                            // before the encoder — see `crate::fps_mode`'s
                            // module doc for what `cfr`/`vfr` actually do and
                            // what they do not reproduce byte-for-byte from
                            // the reference. `passthrough` costs nothing: it
                            // is already what this leg does without it.
                            let frames = if s.media == Some(MediaType::Video) {
                                let mode = s
                                    .graph_opts
                                    .fps_mode
                                    .unwrap_or(crate::fps_mode::FpsMode::Auto);
                                if mode == crate::fps_mode::FpsMode::Passthrough {
                                    frames
                                } else {
                                    let mut video = p
                                        .video
                                        .as_ref()
                                        .ok_or_else(|| {
                                            internal(
                                                "a video stream being transcoded has no video \
                                                 parameters for -fps_mode",
                                            )
                                        })?
                                        .clone();
                                    // `-fps_mode` sits after any pixel-format
                                    // auto-conversion above, so the frames it
                                    // actually receives carry whatever that
                                    // conversion produced, not `p.video`'s own
                                    // pre-conversion format -- `out_video_format`
                                    // is this same function's own source of
                                    // truth for that, already threaded through
                                    // for `out_params` below. Using `p.video`
                                    // unmodified here is exactly the bug this
                                    // fixes: `fps_mode::insert` would declare a
                                    // filter-graph source in the *original*
                                    // format while the frames arriving at it
                                    // are already in the converted one.
                                    if let Some(fmt) = out_video_format {
                                        video.format = Some(fmt);
                                    }
                                    crate::fps_mode::insert(
                                        &mut spec, frames, time_base, &video, mode,
                                    )?
                                }
                            } else {
                                frames
                            };
                            // `-enc_time_base` — see
                            // `crate::enc_time_base`'s module doc for the
                            // measured discrepancy between the reference's
                            // own `--help` and its actual accepted values.
                            let time_base = s.graph_opts.enc_time_base.map_or(time_base, |tb| {
                                tb.resolve(time_base, p.video.as_ref(), p.audio.as_ref())
                            });
                            // Read before `encoder` moves into `add_encoder`
                            // below — see `prime_audio`'s call above for why
                            // this can already be `Some` with no frame sent.
                            let encoder_extradata = encoder.extradata();
                            let packets = spec
                                .add_encoder(frames, encoder, time_base)
                                .map_err(|e| internal_from("could not attach an encoder", &e))?;
                            let mut out_params = encoded_params(&p, encoder_desc.id);
                            if let Some(fmt) = out_video_format
                                && let Some(v) = out_params.video.as_mut()
                            {
                                v.format = Some(fmt);
                            }
                            if let Some((w, h)) = out_video_size
                                && let Some(v) = out_params.video.as_mut()
                            {
                                v.width = w;
                                v.height = h;
                            }
                            if let Some(fmt) = out_audio_format
                                && let Some(a) = out_params.audio.as_mut()
                            {
                                a.format = Some(fmt);
                            }
                            if let Some(rate) = out_audio_rate
                                && let Some(a) = out_params.audio.as_mut()
                            {
                                a.sample_rate = rate;
                            }
                            if let Some(layout) = out_audio_layout
                                && let Some(a) = out_params.audio.as_mut()
                            {
                                a.layout = Some(layout);
                            }
                            out_params.extradata = encoder_extradata;
                            // `nal_length_size` is not asserted beside the
                            // record, it is read out of it — see
                            // `encoded_params`.
                            let nal_length_size = declared_nal_length_size(
                                encoder_desc.id,
                                out_params.extradata.as_deref(),
                            );
                            if let Some(v) = out_params.video.as_mut() {
                                v.nal_length_size = nal_length_size;
                            }
                            (packets, out_params, name.to_owned())
                        }
                    };
                    (packet_tap, out_params, label, format!("{file}:{stream}"))
                }
                StreamPick::Complex(index) => {
                    // Validated in `resolve_output`: a complex-graph-sourced
                    // stream is never `StreamCodec::Copy`.
                    let StreamCodec::Encode(name) = s.codec else {
                        return Err(internal(
                            "a complex filtergraph output must be encoded, not copied",
                        ));
                    };
                    let (frames, time_base, p) = complex_taps
                        .get(*index)
                        .cloned()
                        .ok_or_else(|| internal("a complex filtergraph output was not built"))?;
                    let encoder_desc = vaco_registry::encoder_by_name(name).ok_or_else(|| {
                        internal("the resolved encoder is no longer in the registry")
                    })?;
                    // See the matching comment above `StreamCodec::Encode`'s
                    // own decoder/encoder build: `Limits::permissive()` is the
                    // documented CLI default, not `default()`/`strict()`.
                    let limits = vaco_limits::Limits::permissive();
                    let mut encoder = encoder_desc.build(limits);
                    for (key, value) in &s.codec_options {
                        encoder
                            .set_option(key, value)
                            .map_err(|e| internal_from("the encoder refused an option", &e))?;
                    }
                    let packets = spec
                        .add_encoder(frames, encoder, time_base)
                        .map_err(|e| internal_from("could not attach an encoder", &e))?;
                    (
                        packets,
                        encoded_params(&p, encoder_desc.id),
                        name.to_owned(),
                        format!("complex:{index}"),
                    )
                }
            };
            let mux_stream_index = spec
                .map_with_matrix(packet_tap, oref, &out_params, s.output_matrix)
                .map_err(|e| internal_from("the muxer refused a stream", &e))?;
            // `-bsf`/`-bsf:v`/`-bsf:a`/`-bsf:s`: seeded ahead of whatever the
            // muxer's own `check_bitstream` still asks for (M6) — see
            // `vaco_format_core::mux::MuxWriter::decide_bitstream`'s own doc
            // for why that composes rather than fights, measured against
            // real ffmpeg. Only called when there is something to attach:
            // `set_output_stream_bsf` on an empty chain would be a harmless
            // no-op, but every other mapped stream in this same loop skips
            // it, so an empty stream does too rather than looking special.
            if !s.bsf.is_empty() {
                spec.set_output_stream_bsf(oref, mux_stream_index, s.bsf.clone())
                    .map_err(|e| internal_from("the muxer refused a bitstream filter", &e))?;
            }
            report.mapping.push(format!(
                "  Stream #{mapping_source} -> #{}:{i} ({label})",
                out.index
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

    for (out, (sink, high_water)) in outputs.iter().filter(|o| !o.dropped).zip(sinks) {
        let t = sink.tally();
        // A stream this run actually decoded (not copied) that reached
        // `Finish::Complete` — no node errored — with zero packets muxed is
        // not a quiet no-op. This build has no `-frames` (the one remaining
        // way to legitimately trim a stream to empty now that `-ss`/`-t`
        // exist), so the only way a real decode-then-encode leg produces
        // nothing is that the decode itself silently failed: an SPS-parse
        // failure once did exactly this, completing the whole pipeline and
        // reporting `frame= 0` at exit 0. `t.streams` is built by one
        // `add_stream`/`add_stream_with` call per `out.streams` entry, in
        // that same order (`TallyingMuxer`'s own impl), so the two line up
        // index for index without needing the tally to carry its own
        // `StreamCodec` copy.
        for (spec_stream, stream_tally) in out.streams.iter().zip(&t.streams) {
            let expected_video = matches!(spec_stream.codec, StreamCodec::Encode(_))
                && spec_stream.media == Some(MediaType::Video);
            if expected_video && stream_tally.packets == 0 {
                return Err(Diagnostic::conversion_failed());
            }
        }
        let total_bytes = high_water.map(|h| h.load(Ordering::Relaxed));
        report.summary.push(summary_line(out, &t, total_bytes));
        report.tallies.push(t);
        report.total_bytes.push(total_bytes);
    }
    report.input_duration_secs = input_duration_secs;
    Ok(report)
}

/// Open the real muxer for one output: the registry descriptor's own
/// [`vaco_format_core::MuxerDesc::open`], fed a sink from the protocol layer.
///
/// Returns the total-bytes-written handle alongside the muxer rather than
/// making the caller dig it back out, because there is exactly one point
/// where both are known at once — here, before the sink is boxed away inside
/// the muxer for good.
///
/// # Two constructions for one output, and why
///
/// `MuxerDesc` carries no `flags` field the way `DemuxerDesc` does (compare
/// the two definitions in `vaco_format_core::lib`), so whether a format is
/// `FormatFlags::NOFILE` — `null`, `mkvtimestamp_v2` — is only knowable by
/// asking an *instance*, which means constructing one. The reference never
/// opens a real file for such a format at all (`ffmpeg -f null out.bin` leaves
/// `out.bin` untouched), so the order has to be: construct once against a
/// throwaway in-memory sink to ask, and only open the real protocol sink —
/// which for `file:` truncates on open, a visible side effect — once the
/// answer is "no". Reported as a gap in `docs/app/vaco-cli.md`; a `flags`
/// field on `MuxerDesc` would remove the throwaway construction entirely.
///
/// # Errors
///
/// A [`Diagnostic`] if the descriptor's `open` rejects either sink, or if the
/// protocol layer cannot open the destination (unwrapped so the exit code
/// reflects the real `io::ErrorKind`, matching `input::open`'s side).
fn open_output(
    out: &ResolvedOutput,
    overwrite: crate::overwrite::OverwritePolicy,
) -> Result<(Box<dyn Muxer>, Option<Arc<AtomicU64>>), Diagnostic> {
    let desc = vaco_registry::muxer_by_name(out.format)
        .ok_or_else(|| internal("a resolved output format is no longer in the registry"))?;

    // Deliberately not `MuxerDesc::probe_flags()`, which exists for callers that
    // want only the flags. This one wants the *instance* as well: a `NOFILE`
    // muxer writes nothing, so the throwaway construction against a memory sink
    // is the real muxer and there is no second construction to pay for.
    // `probe_flags` would discard it and we would build twice on the path where
    // building twice is avoidable.
    let mut probe = (desc.open)(Box::new(vaco_format_core::vacoraw::MemorySink::new()))
        .map_err(|e| muxer_open_error(out, &e))?;
    if probe.flags().contains(FormatFlags::NOFILE) {
        // A `NOFILE` muxer has no byte-oriented destination at all, but some
        // (WHIP, `vaco-mux-whip`) still need to know the URL itself —
        // an HTTP endpoint to negotiate against, not a sink to write into.
        // `Muxer::bind_url`'s doc comment names this pattern explicitly:
        // give every `NOFILE` muxer the chance to ask, the same way
        // `NEEDNUMBER` below already does, and treat "I have no use for
        // this" (the default `Unsupported`) as silently fine — which is
        // exactly today's behaviour for `null`/`mkvtimestamp_v2` and every
        // other `NOFILE` muxer that predates this branch.
        match probe.bind_url(&out.url) {
            Ok(()) | Err(Error::Unsupported(_)) => {}
            Err(e) => return Err(muxer_open_error(out, &e)),
        }
        return Ok((probe, None));
    }
    // The destination is a filename *pattern* (`out_%03d.png`), not a real
    // file — `crate::output::create` below would happily create a literal,
    // wrongly-named file for it. Keep this throwaway-sink-backed instance,
    // the same way the `NOFILE` branch above does, and let `Muxer::bind_url`
    // replace its state with the real per-file writer `vaco-mux-image2`
    // already implements correctly.
    if probe.flags().contains(FormatFlags::NEEDNUMBER) {
        probe
            .bind_url(&out.url)
            .map_err(|e| muxer_open_error(out, &e))?;
        return Ok((probe, None));
    }
    drop(probe);

    // `-y`/`-n` (CLI-option audit): checked here, after the `NOFILE`/
    // `NEEDNUMBER` branches above have already returned -- the reference
    // never prompts for a destination it does not actually open as a byte
    // sink either (`ffmpeg -f null -y out.bin` truncates nothing to ask
    // about). Must run before `crate::output::create` below, which is the
    // call that truncates a real file on disk. `overwrite::guard` already
    // returns the reference's own exit-0 `Diagnostic` shape for a refusal,
    // so this propagates it unchanged.
    crate::overwrite::guard(&out.url, overwrite)?;

    let sink = crate::output::create(&out.url).map_err(|e| output_open_error(out, &e))?;
    let counting = crate::output::HighWaterSink::new(sink);
    let high_water = counting.high_water();
    let muxer = (desc.open)(Box::new(counting)).map_err(|e| muxer_open_error(out, &e))?;
    Ok((muxer, Some(high_water)))
}

/// Measured (`ffmpeg 8.1`): opening a real output that permission denies —
/// `ffmpeg -i in.mp4 -c copy -f matroska ro/out.mkv` against a read-only
/// `ro/` — prints
///
/// ```text
/// [out#0/matroska @ 0x…] Error opening output ro/out.mkv: Permission denied
/// Error opening output file ro/out.mkv.
/// Error opening output files: Permission denied
/// ```
///
/// and exits 243 (`EACCES`). `Diagnostic::opening` already produces the last
/// two lines for `what = "output"`; this supplies the first, sans pointer.
fn output_open_error(out: &ResolvedOutput, e: &Error) -> Diagnostic {
    let av = AvError::of(e);
    Diagnostic::opening(
        av,
        vec![format!(
            "[out#{}/{}] Error opening output {}: {}",
            out.index, out.format, out.url, av.text
        )],
        "output",
        &out.url,
    )
}

/// A muxer's own `open`/`init` rejecting the sink it was given — unmeasured
/// against the reference (no probe here forced a real container to refuse
/// construction), so this wording is our own rather than a reproduction.
fn muxer_open_error(out: &ResolvedOutput, e: &Error) -> Diagnostic {
    let av = AvError::of(e);
    Diagnostic::opening(
        av,
        vec![format!(
            "[out#{}/{}] Error opening output {}: {e}",
            out.index, out.format, out.url
        )],
        "output",
        &out.url,
    )
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
///
/// # What it reads when it is *not* `unknown`, measured against `ffmpeg 8.1`
///
/// Remuxing one video+audio file (10 908 payload bytes by
/// `ffprobe -show_entries packet=size`) three ways:
///
/// | destination | total bytes written | printed |
/// |---|---|---|
/// | seekable `.mkv` | 12 168 | `11.551155%` |
/// | seekable `.mp4` | 12 650 | `15.969930%` |
/// | `.mkv` over a real, unseekable pipe | 12 038 | `10.359369%` |
///
/// Every row is `100 * (total − payload) / payload`, printed with six decimal
/// digits — a `%f` conversion, not a rounded percentage. `total` is bytes
/// actually written, not `stat()` of the finished file: the pipe row has no
/// file to `stat`, and still prints a number, which is why [`open_output`]
/// tracks a high-water mark on the sink itself
/// ([`crate::output::HighWaterSink`]) rather than asking the filesystem
/// afterward. `unknown` is reserved for the case that mark never moved at all
/// — a `NOFILE` container, which never touches a sink in the first place — not
/// for "the destination happens to be unseekable".
#[must_use]
pub fn summary_line(out: &ResolvedOutput, t: &OutputTally, total_bytes: Option<u64>) -> String {
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
    let payload: u64 = t.streams.iter().map(|s| s.bytes).sum();
    let overhead = match total_bytes {
        Some(total) if payload > 0 => {
            format!(
                "{:.6}%",
                100.0 * (total as f64 - payload as f64) / payload as f64
            )
        }
        _ => "unknown".to_owned(),
    };
    format!(
        "[out#{}/{}] video:{}KiB audio:{}KiB subtitle:{}KiB other streams:{}KiB global headers:0KiB muxing overhead: {overhead}",
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

/// The format a decode leg must convert to before an encoder will take it, or
/// `None` when it can be fed as it is.
///
/// An empty `accepted` means the encoder does not care. An unknown
/// `source` cannot be ranked against, so the encoder's own first preference is
/// the best guess available — `image2`'s pipe demuxers report no format at
/// probe time, and returning `None` for them skips a conversion that is still
/// needed.
fn converter_target(source: Option<PixFmt>, accepted: &[PixFmt]) -> Option<PixFmt> {
    match source {
        Some(f) => f.best_of(accepted),
        None => accepted.first().copied(),
    }
}

fn internal_from(what: &str, e: &Error) -> Diagnostic {
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

    fn input_with_matrix(matrix: Option<[i32; 9]>) -> select::InputStreams {
        let mut files = select::InputStreams::default();
        files.push_described(0, 16, 12, 0, 0); // one video stream, index 0
        if let Some(slot) = files.display_matrix.first_mut() {
            *slot = matrix;
        }
        files
    }

    /// `(0, -65536, 65536, 0)` is `-display_rotation 90`'s own real,
    /// `ffprobe`-measured matrix (see `dihedral_transform_from_matrix`'s
    /// doc) -- used here as a real container matrix with no CLI override at
    /// all, exercising the "read the input's own tkhd" path.
    const MEASURED_PLUS_90_MATRIX: [i32; 9] = [0, -65536, 0, 65536, 0, 0, 0, 0, 1 << 30];

    #[test]
    fn a_container_matrix_resolves_without_any_display_flag() {
        let (cli, _) = out_of(&["-i", "in.mp4", "-f", "null", "-"]);
        let files = [input_with_matrix(Some(MEASURED_PLUS_90_MATRIX))];
        let got = resolve_rotate(&cli, &files, StreamPick::demuxed(0, 0)).unwrap();
        assert_eq!(
            got,
            Some(vaco_format_core::DisplayTransform::TransposeCclock)
        );
    }

    #[test]
    fn noautorotate_suppresses_a_real_container_matrix() {
        let (cli, _) = out_of(&["-noautorotate", "-i", "in.mp4", "-f", "null", "-"]);
        let files = [input_with_matrix(Some(MEASURED_PLUS_90_MATRIX))];
        let got = resolve_rotate(&cli, &files, StreamPick::demuxed(0, 0)).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn display_rotation_overrides_the_container_matrix_wholesale() {
        // The container says +90 (no flip, matrix above); the CLI override
        // says +90 with a horizontal flip, and per the reference's own
        // documented text ("This option overrides the rotation/display
        // transform metadata stored in the file, if any"), the override
        // wins outright rather than composing with the container's matrix.
        let (cli, _) = out_of(&[
            "-display_rotation",
            "90",
            "-display_hflip",
            "-i",
            "in.mp4",
            "-f",
            "null",
            "-",
        ]);
        let files = [input_with_matrix(Some(MEASURED_PLUS_90_MATRIX))];
        let got = resolve_rotate(&cli, &files, StreamPick::demuxed(0, 0)).unwrap();
        assert_eq!(
            got,
            Some(vaco_format_core::DisplayTransform::TransposeClockFlip)
        );
    }

    #[test]
    fn no_matrix_and_no_override_is_nothing_to_rotate() {
        let (cli, _) = out_of(&["-i", "in.mp4", "-f", "null", "-"]);
        let files = [input_with_matrix(None)];
        let got = resolve_rotate(&cli, &files, StreamPick::demuxed(0, 0)).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn a_complex_graph_source_has_no_input_stream_to_rotate() {
        let (cli, _) = out_of(&["-i", "in.mp4", "-f", "null", "-"]);
        let files = [input_with_matrix(Some(MEASURED_PLUS_90_MATRIX))];
        let got = resolve_rotate(&cli, &files, StreamPick::Complex(0)).unwrap();
        assert_eq!(got, None);
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
    fn disposition_expression_adds_named_bit_values() {
        // Measured shape (`vaco_core::Disposition`'s own module doc): the
        // reference resolves `-disposition` through its expression
        // evaluator, so `"default+original"` is plain arithmetic on the two
        // flags' bit values, not a `+`/`-` flag-list grammar.
        let d = eval_disposition("default+original").unwrap();
        assert!(d.contains(Disposition::DEFAULT));
        assert!(d.contains(Disposition::ORIGINAL));
        assert!(!d.contains(Disposition::FORCED));
    }

    #[test]
    fn disposition_expression_numeric_literal_is_a_bitmask() {
        let d = eval_disposition("0").unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn disposition_expression_rejects_an_unknown_name() {
        assert!(eval_disposition("nonesuch").is_err());
    }

    #[test]
    fn disposition_expression_is_case_sensitive_like_the_reference() {
        // vaco_core::Disposition's own doc: `DEFAULT`/`Default` are rejected,
        // only `default` is a bound name.
        assert!(eval_disposition("DEFAULT").is_err());
    }

    #[test]
    fn program_grammar_collects_title_and_repeated_st() {
        let p = parse_program("title=Commentary:st=0:st=2").unwrap();
        assert_eq!(p.stream_indices, vec![0, 2]);
        assert!(
            p.metadata
                .contains(&("title".to_owned(), "Commentary".to_owned()))
        );
    }

    #[test]
    fn program_grammar_rejects_an_unrecognized_field() {
        assert!(parse_program("bogus=1").is_err());
    }

    #[test]
    fn program_grammar_rejects_a_non_numeric_stream_index() {
        assert!(parse_program("st=zz").is_err());
    }

    #[test]
    fn cli_wires_disposition_and_program_into_mux_metadata() {
        let (c, o) = out_of(&[
            "-i",
            "a.mkv",
            "-map",
            "0:v",
            "-disposition:v",
            "default+forced",
            "-program",
            "title=Main:st=0",
            "out.mkv",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::Demuxed { file: 0, stream: 0 },
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        let meta = metadata_of(&c, &o, &streams).unwrap();
        assert!(
            meta.disposition_for_stream(0)
                .contains(Disposition::DEFAULT)
        );
        assert!(meta.disposition_for_stream(0).contains(Disposition::FORCED));
        assert_eq!(meta.programs.len(), 1);
        let program = meta.programs.first().unwrap();
        assert_eq!(program.stream_indices, vec![0]);
        assert!(
            program
                .metadata
                .contains(&("title".to_owned(), "Main".to_owned()))
        );
    }

    #[test]
    fn f_null_resolves_to_the_null_muxer() {
        let (_, o) = out_of(&["-i", "a.mkv", "-f", "null", "-"]);
        assert_eq!(muxer_for(&o).unwrap(), "null");
    }

    /// `-f image2 'out_%03d.png'` used to have `open_output` create a real,
    /// literal file named `out_%03d.png` (`crate::output::create`,
    /// unconditionally, before this) instead of ever reaching the per-frame
    /// writer. The `NEEDNUMBER` branch added above keeps the
    /// throwaway-sink-backed probe instance and rebinds it, so no file at the
    /// literal pattern name is ever created and the real per-frame writer
    /// (`vaco-mux-image2`) is what `run_pipeline` ends up writing through.
    #[test]
    fn f_image2_pattern_output_binds_instead_of_creating_a_literal_file() {
        let dir =
            std::env::temp_dir().join(format!("vaco-cli-exec-test-image2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pattern = dir.join("out%03d.png");
        let pattern = pattern.to_str().unwrap();

        let out = ResolvedOutput {
            index: 0,
            url: pattern.to_owned(),
            format: "image2",
            streams: Vec::new(),
            sink: Sink::new(),
            dropped: false,
            metadata: MuxMetadata::default(),
            map_chapters: None,
            map_metadata: None,
            format_opts: vaco_format_core::FormatOptions::default(),
        };
        let (mut muxer, high_water) =
            open_output(&out, crate::overwrite::OverwritePolicy::Always).unwrap();
        assert!(high_water.is_none());
        // Never created under the literal, unexpanded name.
        assert!(!dir.join("out%03d.png").exists());

        muxer
            .add_stream(&vaco_codec_core::CodecParameters::new(MediaType::Video))
            .unwrap();
        muxer.write_header().unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let p = vaco_packet::Packet::from_slice(&mut budget, b"frame-one").unwrap();
        muxer.write_packet(&p).unwrap();
        muxer.write_trailer().unwrap();
        assert_eq!(std::fs::read(dir.join("out001.png")).unwrap(), b"frame-one");

        let _ = std::fs::remove_dir_all(&dir);
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

    /// E2E-GAPS 4: `-f null -` with no `-c` on either stream now resolves to
    /// the container's own declared defaults — `wrapped_avframe` for video,
    /// `pcm_s16le` for audio — the same pair the reference (`ffmpeg 9.0.1`,
    /// measured) maps a decodable h264/aac input to when neither `-c:v` nor
    /// `-c:a` is given. This replaces a test that asserted the *absence* of
    /// that mapping, which is exactly the "pin the absence of something the
    /// project is building" trap: it was true only because `check_codecs`
    /// never consulted
    /// `MuxerDesc::default_video`/`default_audio` at all, not because the
    /// container has no default.
    #[test]
    fn null_video_and_audio_streams_pick_up_the_containers_declared_default_encoder() {
        let (c, mut o) = out_of(&["-i", "a.mkv", "-f", "null", "-"]);
        o.format = Some("null".to_owned());
        let video = OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        };
        let audio = OutStream {
            media: Some(MediaType::Audio),
            ..video.clone()
        };
        let resolved = check_codecs(&c, &o, "null", &[video, audio]).unwrap();
        assert_eq!(
            resolved,
            vec![
                StreamCodec::Encode("wrapped_avframe"),
                StreamCodec::Encode("pcm_s16le"),
            ]
        );
    }

    /// The diagnosis this crate had for every `-c`-less stream before
    /// E2E-GAPS 4 is still exactly right for a media type the resolved
    /// container declares no default for at all — a subtitle stream on
    /// `-f null -`, here — so the reference's own message is still reachable,
    /// just from a narrower gate.
    #[test]
    fn a_stream_with_no_codec_and_no_declared_default_takes_the_reference_missing_encoder_path() {
        let (c, mut o) = out_of(&["-i", "a.mkv", "-f", "null", "-"]);
        o.format = Some("null".to_owned());
        let s = OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Subtitle),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        };
        let e = check_codecs(&c, &o, "null", &[s]).unwrap_err();
        assert_eq!(
            e.render(),
            "[sost#0:0] Automatic encoder selection failed Default encoder for format null (codec none) is probably disabled. Please choose an encoder manually.\n\
             [sost#0:0] Error selecting an encoder\n\
             Error opening output file -.\n\
             Error opening output files: Encoder not found\n"
        );
        assert_eq!(e.exit.code(), 8);
    }

    #[test]
    fn copy_is_accepted_and_a_named_encoder_is_not() {
        let (c, o) = out_of(&["-i", "a.mkv", "-c", "copy", "-f", "null", "-"]);
        let s = OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        };
        assert!(check_codecs(&c, &o, "null", std::slice::from_ref(&s)).is_ok());

        // Deliberately a name no encoder will ever have. `libx264` used to
        // stand in for "not registered here" and stopped being true the moment
        // vaco-codec-exec landed it (C-46), which spawns the user's own binary.
        let (c, o) = out_of(&["-i", "a.mkv", "-c:v", "libnosuchencoder", "-f", "null", "-"]);
        let e = check_codecs(&c, &o, "null", &[s]).unwrap_err();
        assert!(
            e.render()
                .starts_with("[vost#0:0] Unknown encoder 'libnosuchencoder'\n"),
            "{}",
            e.render()
        );
        assert_eq!(e.exit.code(), 8);
    }

    /// A registered encoder name resolves through
    /// `vaco_registry::encoder_by_name` instead of hitting the "Unknown
    /// encoder" catch-all. `qoi` is always in this build's default features.
    #[test]
    fn an_unknown_source_format_still_gets_a_converter() {
        // image2's pipe demuxers report no format at probe time. Ranking is
        // impossible without a source, but skipping the conversion is worse
        // than guessing: the encoder's first preference is the guess.
        let accepted = [PixFmt::Rgb24, PixFmt::Gray8];
        assert_eq!(converter_target(None, &accepted), Some(PixFmt::Rgb24));
    }

    #[test]
    fn an_encoder_that_accepts_anything_never_gets_a_converter() {
        assert_eq!(converter_target(None, &[]), None);
        assert_eq!(converter_target(Some(PixFmt::Rgb24), &[]), None);
    }

    #[test]
    fn a_known_source_is_ranked_rather_than_taking_the_first_entry() {
        let accepted = [PixFmt::MonoBlack, PixFmt::Rgb24];
        assert_eq!(
            converter_target(Some(PixFmt::Bgr24), &accepted),
            Some(PixFmt::Rgb24)
        );
    }

    #[test]
    fn a_registered_encoder_name_resolves_through_the_registry() {
        let (c, o) = out_of(&["-i", "a.mkv", "-c:v", "qoi", "-f", "null", "-"]);
        let s = OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        };
        let resolved = check_codecs(&c, &o, "null", &[s]).unwrap();
        assert_eq!(resolved, vec![StreamCodec::Encode("qoi")]);
    }

    #[test]
    fn codec_options_of_passes_through_a_private_encoder_option_verbatim() {
        // `-pred paeth` on a PNG output: unlike `-b`/`-q`, this has no CLI-side
        // grammar of its own, so `codec_options_of` must hand the raw string
        // straight to `Encoder::set_option` rather than evaluating it.
        let (c, o) = out_of(&[
            "-i", "a.png", "-c:v", "png", "-pred", "paeth", "-f", "image2", "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Encode("png"),
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        let opts = codec_options_of(&c, &o, &streams).unwrap();
        assert_eq!(opts, vec![vec![("pred".to_owned(), "paeth".to_owned())]]);
    }

    #[test]
    fn bsf_options_of_resolves_a_bare_filter_by_name() {
        let (c, o) = out_of(&[
            "-i",
            "a.mp4",
            "-c",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "mpegts",
            "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        let chains = bsf_options_of(&c, &o, &streams).unwrap();
        assert_eq!(
            chains,
            vec![vec![vaco_format_core::mux::UserBsf {
                name: "h264_mp4toannexb",
                options: Vec::new(),
            }]]
        );
    }

    #[test]
    fn bsf_options_of_parses_a_comma_separated_chain() {
        let (c, o) = out_of(&[
            "-i",
            "a.mp4",
            "-c",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb,dump_extra",
            "-f",
            "mpegts",
            "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        let chains = bsf_options_of(&c, &o, &streams).unwrap();
        assert_eq!(
            chains.first().unwrap().iter().map(|b| b.name).collect::<Vec<_>>(),
            vec!["h264_mp4toannexb", "dump_extra"]
        );
    }

    #[test]
    fn bsf_options_of_refuses_an_unknown_filter_name() {
        let (c, o) = out_of(&[
            "-i",
            "a.mp4",
            "-c",
            "copy",
            "-bsf:v",
            "nosuchfilterxyz",
            "-f",
            "mpegts",
            "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        assert!(bsf_options_of(&c, &o, &streams).is_err());
    }

    #[test]
    fn bsf_options_of_refuses_a_filter_given_options() {
        // Gap 12: `BsfProvider::open` has nowhere to put them, so this is
        // refused by name rather than silently opened bare.
        let (c, o) = out_of(&[
            "-i",
            "a.mp4",
            "-c",
            "copy",
            "-bsf:v",
            "h264_metadata=aud=insert",
            "-f",
            "mpegts",
            "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        assert!(bsf_options_of(&c, &o, &streams).is_err());
    }

    #[test]
    fn last_match_wins_for_bsf_too() {
        // Same rule `last_match_wins_across_per_stream_codec_options` proves
        // for `-c:a`: `-bsf:v:0 <x> -bsf:v <y>` gives stream v:0 `y`.
        let (c, o) = out_of(&[
            "-i",
            "a.mp4",
            "-c",
            "copy",
            "-bsf:v:0",
            "dump_extra",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "mpegts",
            "-",
        ]);
        let streams = vec![OutStream {
            source: StreamPick::demuxed(0, 0),
            media: Some(MediaType::Video),
            codec: StreamCodec::Copy,
            graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
            force_key_frames: None,
            codec_options: Vec::new(),
            output_matrix: None,
            bsf: Vec::new(),
        }];
        let chains = bsf_options_of(&c, &o, &streams).unwrap();
        assert_eq!(
            chains,
            vec![vec![vaco_format_core::mux::UserBsf {
                name: "h264_mp4toannexb",
                options: Vec::new(),
            }]]
        );
    }

    #[test]
    fn last_match_wins_across_per_stream_codec_options() {
        // `-c:a:1 flac -c:a copy` gives stream a:1 `copy`, not `flac`.
        let (c, o) = out_of(&[
            "-i", "a.mkv", "-c:a:1", "flac", "-c:a", "copy", "-f", "null", "-",
        ]);
        let streams = vec![
            OutStream {
                source: StreamPick::demuxed(0, 0),
                media: Some(MediaType::Audio),
                codec: StreamCodec::Copy,
                graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
                force_key_frames: None,
                codec_options: Vec::new(),
                output_matrix: None,
                bsf: Vec::new(),
            },
            OutStream {
                source: StreamPick::demuxed(0, 1),
                media: Some(MediaType::Audio),
                codec: StreamCodec::Copy,
                graph_opts: crate::filtergraph::SimpleGraphOptions::default(),
                force_key_frames: None,
                codec_options: Vec::new(),
                output_matrix: None,
                bsf: Vec::new(),
            },
        ];
        assert!(check_codecs(&c, &o, "null", &streams).is_ok());
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
            metadata: MuxMetadata::default(),
            map_chapters: None,
            map_metadata: None,
            format_opts: vaco_format_core::FormatOptions::default(),
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
            summary_line(&out, &t, None),
            "[out#0/null] video:7KiB audio:16KiB subtitle:0KiB other streams:0KiB global headers:0KiB muxing overhead: unknown"
        );
        // The three measured points, and the tie the two rounding rules
        // disagree on.
        assert_eq!(kib_of(7459), 7);
        assert_eq!(kib_of(16_354), 16);
        assert_eq!(kib_of(8992), 9);
        assert_eq!(kib_of(1536), 2, "1.5 KiB ties to even");
    }

    #[test]
    fn the_summary_line_computes_overhead_from_bytes_actually_written() {
        // Measured against `ffmpeg 8.1`: a 10 908-byte payload remuxed to a
        // seekable `.mkv` produces a 12 168-byte file and prints
        // `11.551155%`. See `summary_line`'s docs for the other two rows this
        // was cross-checked against.
        let out = ResolvedOutput {
            index: 0,
            url: "out.mkv".to_owned(),
            format: "matroska",
            streams: Vec::new(),
            sink: Sink::new(),
            dropped: false,
            metadata: MuxMetadata::default(),
            map_chapters: None,
            map_metadata: None,
            format_opts: vaco_format_core::FormatOptions::default(),
        };
        let t = OutputTally {
            streams: vec![
                crate::nullmux::StreamTally {
                    media: Some(MediaType::Video),
                    packets: 5,
                    bytes: 2048,
                },
                crate::nullmux::StreamTally {
                    media: Some(MediaType::Audio),
                    packets: 45,
                    bytes: 8860,
                },
            ],
            header_written: true,
            trailer_written: true,
        };
        assert_eq!(
            summary_line(&out, &t, Some(12_168)),
            "[out#0/matroska] video:2KiB audio:9KiB subtitle:0KiB other streams:0KiB global headers:0KiB muxing overhead: 11.551155%"
        );
        assert_eq!(kib_of(2560), 2, "2.5 KiB ties to even");
        assert_eq!(kib_of(0), 0);
    }
}
