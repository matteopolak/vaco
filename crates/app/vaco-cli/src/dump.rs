//! `Input #0, from '…':`/`Output #0, to '…':` — the container dump the
//! reference prints before it does anything else, plus the `Output`
//! side's own header (part of CL-17's stderr surface).
//!
//! # Why this exists
//!
//! `ffmpeg -hide_banner -i in.mp4` with no output prints the whole `Input #0`
//! block and then the "no output" error; before this module `vaco` printed
//! only the error. The measured reference transcript used to build this covers
//! three fixtures:
//! an MP4 with one video stream, an MP4 with video+audio, and an MPEG-TS file
//! with a `Program` block. Every rule below cites which of those three (or a
//! throwaway file built for the purpose) it was measured on.
//!
//! # What is *not* measured here
//!
//! - Color-description parentheticals (`(tv, bt709, progressive)`): none of
//!   the three fixtures carry non-default color info, so only [`FieldOrder`]
//!   is reproduced. A file with explicit primaries/trc/space would print a
//!   richer parenthetical this does not yet build.
//! - Multi-line metadata values (an embedded newline continues the value on
//!   a fresh line at the same key column in the reference). Not implemented;
//!   no fixture needed it.
//! - The `Output #0` side's `tbr`/`tbn` are the **source** stream's, not the
//!   muxer's own choice — measured on `long.mp4 -> mpegts`, the reference
//!   changes `12800 tbn` to `90k tbn` on the output line because the muxer
//!   picks its own time base before `write_header`. Reaching that value
//!   would mean asking the opened muxer after stream setup, which nothing
//!   here has a handle on yet; reported rather than faked. Likewise the
//!   `Output` line never carries `-map_metadata`'s copied-from-input tags,
//!   only this output's own explicit `-metadata`, and never a `q=…` segment
//!   (there is no encoder in this build for that number to describe).
//! - `Output #0`/`Press [q]…` print *before* the pipeline runs in the
//!   reference; here they print before [`crate::exec::run_pipeline`] is
//!   called but after `Stream mapping:`/the muxing-overhead summary are
//!   already queued to print *after* it returns, so the two blocks are not
//!   perfectly interleaved with the reference's own order. Not exercised by
//!   the `-i F` (no output) diff loop this was graded against.

use core::fmt::Write as _;

use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, VideoParameters};
use vaco_core::{Disposition, MediaType, Rational};
use vaco_format_core::{Demuxer, DemuxerDesc, Program, Stream};
use vaco_sampfmt::SampleFmt;

use crate::exec::ResolvedOutput;
use crate::input::InputFile;
use crate::select::StreamPick;

// ------------------------------------------------------------------ input

/// Render one `Input #N, <format>, from '<url>':` block.
///
/// `size` is the input's byte length when the transport can report one
/// (`None` for a pipe), which is what the container-level `bitrate:` field is
/// computed from — the reference does the same thing `vaco-probe`'s
/// `-show_format` already does for the same field, from a file size and a
/// duration, truncated rather than rounded.
#[must_use]
pub fn render_input(
    index: u32,
    url: &str,
    desc: &DemuxerDesc,
    demuxer: &dyn Demuxer,
    size: Option<u64>,
) -> Vec<String> {
    let mut out = vec![format!("Input #{index}, {}, from '{url}':", desc.name)];
    metadata_block(&mut out, 2, demuxer.metadata());
    out.push(duration_line(demuxer, size));

    let streams = demuxer.streams();
    let mut used = vec![false; streams.len()];
    for p in demuxer.programs() {
        out.push(program_header(p));
        metadata_block(&mut out, 4, &p.metadata);
        for &si in &p.stream_indices {
            if let Some(pos) = streams.iter().position(|s| s.index == si)
                && let Some(flag) = used.get_mut(pos)
                && let Some(s) = streams.get(pos)
            {
                *flag = true;
                push_stream(&mut out, index, s);
            }
        }
    }
    for (i, s) in streams.iter().enumerate() {
        if !used.get(i).copied().unwrap_or(false) {
            push_stream(&mut out, index, s);
        }
    }
    out
}

fn push_stream(out: &mut Vec<String>, file_index: u32, s: &Stream) {
    out.push(stream_line(file_index, s));
    metadata_block(out, 4, &s.metadata);
}

/// `Program N ` — trailing space and all. Measured on `long.ts`: `Program 1 `
/// with nothing after the space but the newline.
fn program_header(p: &Program) -> String {
    format!("  Program {} ", p.program_num.unwrap_or(p.id))
}

/// `  Duration: …, start: …, bitrate: …`.
///
/// `start` is omitted entirely when no stream states one — measured on
/// `a.wav`, which has neither a `start:` field nor any per-stream start; it is
/// not "0.000000 hidden", the reference's own duration line simply stops
/// after `Duration:` there. `bitrate` prints `N/A` when it cannot be computed
/// (no known size, or no known duration).
fn duration_line(demuxer: &dyn Demuxer, size: Option<u64>) -> String {
    let duration = demuxer.duration();
    let start = vaco_format_core::time::container_start_time(
        demuxer
            .streams()
            .iter()
            .map(|s| (s.start_time, s.time_base)),
    );

    let mut line = String::from("  Duration: ");
    line.push_str(&duration.map_or_else(|| "N/A".to_owned(), |d| clock(secs(d))));
    if let Some(s) = start {
        let _ = write!(line, ", start: {:.6}", secs(s));
    }
    line.push_str(", bitrate: ");
    let bit_rate = match (size, duration) {
        (Some(sz), Some(d)) if secs(d) > 0.0 => {
            let raw = (sz as f64) * 8.0 / secs(d);
            raw.is_finite().then(|| raw.trunc())
        }
        _ => None,
    };
    line.push_str(&bit_rate.map_or_else(
        || "N/A".to_owned(),
        |b| format!("{} kb/s", (b / 1000.0).trunc() as i64),
    ));
    line
}

// ----------------------------------------------------------------- output

/// Render one `Output #N, <format>, to '<url>':` block.
///
/// See the module docs for what this does not attempt to reproduce: the
/// muxer's own `tbr`/`tbn`, any `-map_metadata`-copied tag, and `q=…`.
#[must_use]
pub fn render_output(out: &ResolvedOutput, inputs: &[InputFile]) -> Vec<String> {
    let mut lines = vec![format!(
        "Output #{}, {}, to '{}':",
        out.index, out.format, out.url
    )];
    metadata_block(&mut lines, 2, &out.metadata.tags);
    for (i, s) in out.streams.iter().enumerate() {
        let Some(src) = source_stream(inputs, s.source) else {
            continue;
        };
        lines.push(output_stream_line(out.index, i as u32, src));
        let tags = out
            .metadata
            .stream_tags
            .get(i)
            .map_or(&[][..], Vec::as_slice);
        metadata_block(&mut lines, 4, tags);
    }
    lines
}

fn source_stream(inputs: &[InputFile], pick: StreamPick) -> Option<&Stream> {
    // A complex-graph-sourced output stream has no real demuxed `Stream` to
    // describe here; the caller already skips it (see `render_output`).
    let (file, stream) = pick.as_demuxed()?;
    inputs
        .get(file as usize)?
        .demuxer
        .streams()
        .iter()
        .find(|s| s.index == stream)
}

fn output_stream_line(file_index: u32, stream_index: u32, s: &Stream) -> String {
    let mut line = format!("  Stream #{file_index}:{stream_index}");
    if let Some(lang) = s.metadata_get("language") {
        let _ = write!(line, "({lang})");
    }
    line.push_str(": ");
    line.push_str(&codec_summary(s));
    line.push_str(&disposition_parens(s.disposition));
    line
}

// -------------------------------------------------------------- streams

fn stream_line(file_index: u32, s: &Stream) -> String {
    let mut line = format!("  Stream #{file_index}:{}", s.index);
    if let Some(id) = s.id {
        let _ = write!(line, "[0x{id:x}]");
    }
    if let Some(lang) = s.metadata_get("language") {
        let _ = write!(line, "({lang})");
    }
    line.push_str(": ");
    line.push_str(&codec_summary(s));
    // Measured on `long.ts`: the video stream's own start (1.48s, distinct
    // from the container-level `start:`) is appended after `tbn` as
    // `, start 1.480000` — no colon, unlike the container-level field.
    // `long.mp4`'s stream start is `0` and is not shown, so the gate is
    // "defined and nonzero" rather than "defined".
    if let Some(d) = s.start_time_absolute()
        && d.as_micros() != 0
    {
        let _ = write!(line, ", start {:.6}", secs(d));
    }
    line.push_str(&disposition_parens(s.disposition));
    line
}

fn codec_summary(s: &Stream) -> String {
    let p = &s.params;
    match s.media_type() {
        Some(MediaType::Video) => video_summary(s, p),
        Some(MediaType::Audio) => audio_summary(p),
        Some(MediaType::Subtitle) => format!("Subtitle: {}", codec_name(p)),
        Some(MediaType::Data) => format!("Data: {}", codec_name(p)),
        _ => format!("Unknown: {}", codec_name(p)),
    }
}

fn codec_name(p: &CodecParameters) -> &'static str {
    p.codec_id.map_or("unknown", CodecId::name)
}

fn video_summary(stream: &Stream, params: &CodecParameters) -> String {
    let video = params.video.as_ref();
    let mut out = String::from("Video: ");
    out.push_str(codec_name(params));
    out.push_str(&profile_paren(params));
    out.push_str(&tag_paren(params));
    out.push_str(", ");
    out.push_str(&pixfmt_segment(video));
    out.push_str(", ");
    let (w, h) = video.map_or((0, 0), |video| (video.width, video.height));
    let _ = write!(out, "{w}x{h}");
    out.push_str(&sar_dar_bracket(video));
    if let Some(br) = params.bit_rate {
        let _ = write!(out, ", {} kb/s", (br as f64 / 1000.0).trunc() as u64);
    }
    let _ = write!(out, ", {} fps", rate_str(stream.avg_frame_rate));
    let _ = write!(out, ", {} tbr", rate_str(stream.r_frame_rate));
    let _ = write!(out, ", {} tbn", tbn_str(stream.time_base));
    out
}

fn audio_summary(p: &CodecParameters) -> String {
    let a = p.audio.as_ref();
    let mut out = String::from("Audio: ");
    out.push_str(codec_name(p));
    out.push_str(&profile_paren(p));
    out.push_str(&tag_paren(p));
    let _ = write!(out, ", {} Hz", a.map_or(0, |a| a.sample_rate));
    out.push_str(", ");
    out.push_str(&a.and_then(|a| a.layout.as_ref()).map_or_else(
        || "unknown".to_owned(),
        |l| {
            l.name()
                .map_or_else(|| format!("{} channels", l.channels), str::to_owned)
        },
    ));
    out.push_str(", ");
    out.push_str(a.and_then(|a| a.format).map_or("none", SampleFmt::name));
    if let Some(br) = p.bit_rate {
        let _ = write!(out, ", {} kb/s", (br as f64 / 1000.0).trunc() as u64);
    }
    out
}

/// ` (High)`, or nothing when the codec has no profile — measured: `flac` and
/// `pcm_s16le` print no profile parenthetical at all, not an empty one.
///
/// Does not reproduce `-bitexact`'s numeric-profile swap (`vaco-probe`'s
/// `show::stream_value` does, for `-show_streams`); unmeasured for the
/// `av_dump_format`-style line this feeds and not exercised by the `-i F`
/// (no `-bitexact`) diff loop this module was built against.
fn profile_paren(p: &CodecParameters) -> String {
    p.profile
        .filter(|pr| !pr.name.is_empty())
        .map_or_else(String::new, |pr| format!(" ({})", pr.name))
}

/// ` (avc1 / 0x31637661)`, or ` ([27][0][0][0] / 0x001B)`, or nothing when the
/// container states no tag at all (`flac`).
///
/// The uppercase hex is deliberate and distinct from `ffprobe`'s own
/// `codec_tag` field (`vaco_textformat::num::codec_tag`, lowercase): measured
/// side by side, `long.ts`'s stream line prints `0x001B` here and `ffprobe
/// -show_streams` prints `codec_tag=0x001b` on the same file. Two writers,
/// two cases — not the same rule reused.
fn tag_paren(p: &CodecParameters) -> String {
    let Some(bytes) = p.codec_tag else {
        return String::new();
    };
    let label = fourcc_label(bytes);
    let value = u32::from_le_bytes(bytes);
    format!(" ({label} / 0x{value:04X})")
}

/// `avc1` for four printable bytes, `[27][0][0][0]` for four that are not.
///
/// Measured both ways on the same session: MP4's H.264 track tag is ASCII
/// (`avc1`); MPEG-TS has no four-character tag at all and reports its
/// `stream_type` byte (27, decimal) through the same four-byte slot, which
/// prints as `[27][0][0][0]`.
fn fourcc_label(bytes: [u8; 4]) -> String {
    let mut out = String::new();
    for b in bytes {
        if b.is_ascii_graphic() || b == b' ' {
            out.push(char::from(b));
        } else {
            out.push('[');
            out.push_str(&b.to_string());
            out.push(']');
        }
    }
    out
}

/// `yuv420p(progressive)`, or the bare name when the field order is not
/// stated.
fn pixfmt_segment(v: Option<&VideoParameters>) -> String {
    let Some(v) = v else {
        return "none".to_owned();
    };
    let Some(fmt) = v.format else {
        return "none".to_owned();
    };
    let mut out = fmt.name().to_owned();
    out.push_str(&field_order_suffix(v.field_order));
    out
}

fn field_order_suffix(order: FieldOrder) -> String {
    let word = match order {
        FieldOrder::Progressive => "progressive",
        FieldOrder::TopFirst => "tt",
        FieldOrder::BottomFirst => "bb",
        FieldOrder::TopCodedFirst => "tb",
        FieldOrder::BottomCodedFirst => "bt",
        FieldOrder::Unknown => return String::new(),
    };
    format!("({word})")
}

/// ` [SAR 1:1 DAR 1:1]`, or nothing when the sample aspect ratio is undefined.
fn sar_dar_bracket(v: Option<&VideoParameters>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    let sar = v.sample_aspect_ratio;
    if !sar.is_defined() || sar.is_zero() {
        return String::new();
    }
    let sar_r = sar.reduced();
    let dar_r = display_aspect(v.width, v.height, sar).reduced();
    format!(
        " [SAR {}:{} DAR {}:{}]",
        sar_r.num, sar_r.den, dar_r.num, dar_r.den
    )
}

/// `width * sar / height`, reduced. Same formula `vaco-probe`'s
/// `display_aspect_ratio` field uses (measured against the same reference),
/// reconstructed here rather than imported: it is three lines and
/// `vaco-probe`'s copy is private to a file another agent is mid-edit on.
fn display_aspect(width: u32, height: u32, sar: Rational) -> Rational {
    let w = i32::try_from(width).unwrap_or(i32::MAX);
    let h = i32::try_from(height).unwrap_or(i32::MAX);
    if h == 0 {
        return Rational::UNDEFINED;
    }
    Rational::new(w, 1) * sar / Rational::new(h, 1)
}

/// `25`, `29.97`, `142.86` — an integral value prints with no decimals at
/// all, anything else prints with exactly two. Measured across four frame
/// rates (`25/1`, `875/38`, `30000/1001`, `1000/7`): every non-integral one
/// printed exactly two decimal digits, rounded rather than truncated
/// (`142.857142… -> 142.86`).
fn rate_str(r: Rational) -> String {
    let v = r.to_f64();
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.2}");
    s.strip_suffix(".00").map_or(s.clone(), str::to_owned)
}

/// `12800`, or `90k`/`30k` when the time base denominator is an exact
/// multiple of 1000. Measured: MP4's `1/12800` prints `12800 tbn` in full;
/// MPEG-TS's `1/90000` and a synthesised `1/30000` both print with the `k`
/// suffix.
fn tbn_str(time_base: Rational) -> String {
    let den = time_base.den;
    if den != 0 && den % 1000 == 0 {
        format!("{}k", (f64::from(den) / 1000.0) as i64)
    } else {
        format!("{den}")
    }
}

/// ` (default) (forced)` — one space-prefixed parenthetical per active flag,
/// in [`Disposition::ALL`]'s bit order. Measured by muxing a file with
/// `-disposition:v default+forced`: the reference prints both, space
/// separated, not comma-joined into one parenthetical.
fn disposition_parens(d: Disposition) -> String {
    let mut out = String::new();
    for &(flag, name) in Disposition::ALL {
        if d.contains(flag) {
            let _ = write!(out, " ({name})");
        }
    }
    out
}

// -------------------------------------------------------------- metadata

/// Keys this block never prints, because the reference does not carry them as
/// `AVDictionary` metadata even though `vaco-probe`'s `-show_streams` prints
/// them (as dedicated fields, not `TAG:`-prefixed ones — confirmed against
/// `ffprobe -of json`, where `ts_id`/`ts_packetsize` sit at the top level of
/// the stream object, not inside its `tags` map). `language` surfaces on the
/// stream/program line instead of here; `ts_id`/`ts_packetsize` are MPEG-TS's
/// own struct fields (`pmt_pid`-shaped facts about the stream, not something
/// PMT descriptors label as metadata) that this workspace's `Stream` model
/// happens to carry in the same generic tag list `vaco-demux-mpegts` fills
/// for its `-show_streams` fields. Measured: `long.ts`'s dump has no
/// `Metadata:` block under its (sole, tagless) stream at all.
const NOT_REALLY_METADATA: &[&str] = &["language", "ts_id", "ts_packetsize"];

/// One `Metadata:` block at `header_indent`, its keys two deeper — or nothing
/// at all when every tag is in [`NOT_REALLY_METADATA`].
fn metadata_block(out: &mut Vec<String>, header_indent: usize, tags: &[(String, String)]) {
    let visible: Vec<&(String, String)> = tags
        .iter()
        .filter(|(k, _)| {
            !NOT_REALLY_METADATA
                .iter()
                .any(|n| k.eq_ignore_ascii_case(n))
        })
        .collect();
    if visible.is_empty() {
        return;
    }
    let header_pad = " ".repeat(header_indent);
    out.push(format!("{header_pad}Metadata:"));
    // The key field's minimum width. Measured: `major_brand` (11 chars) pads
    // to `major_brand     : isom` (5 spaces), `minor_version` (13 chars) pads
    // with 3, and `compatible_brands` (18 chars, on the same file) gets none
    // at all — a plain `%-16s`, not a block-relative alignment. Cross-checked
    // on `long.ts`'s `service_name` (12 chars, 4 spaces) and
    // `service_provider` (exactly 16, no padding).
    let key_pad = " ".repeat(header_indent + 2);
    for (k, v) in visible {
        out.push(format!("{key_pad}{k:<16}: {v}"));
    }
}

// ------------------------------------------------------------------- time

fn secs(d: vaco_core::Duration) -> f64 {
    d.as_micros() as f64 / 1_000_000.0
}

/// `HH:MM:SS.cc` — zero-padded hours, two-digit centiseconds. Distinct from
/// `vaco_textformat::num::sexagesimal` (that one is `-sexagesimal`'s
/// microsecond, non-zero-padded-hour spelling for `ffprobe` fields); this is
/// the plain-clock format the reference's own `Duration:` line and `-stats`'s
/// `time=` field both use, measured on `long.mp4`/`long.ts` (`00:00:06.00`)
/// and on a synthesised 1.237s clip (`00:00:01.24`).
fn clock(seconds: f64) -> String {
    let (h, m, s, cs) = split_clock(seconds);
    format!("{h:02}:{m:02}:{s:02}.{cs:02}")
}

/// [`clock`], for `-stats`'s `time=` field — same `HH:MM:SS.cc` shape,
/// exposed to [`crate::stats`] under its own name so that module does not
/// reach past this one's public/private line for an implementation detail.
#[must_use]
pub(crate) fn clock_for_stats(seconds: f64) -> String {
    clock(seconds)
}

/// `-stats`'s `elapsed=` field: the same clock, but with an **un-padded**
/// hour, matching the measured `elapsed=0:00:00.00` (one digit, no leading
/// zero) rather than `Duration:`'s `00:...`. The two fields are not the same
/// format string in the reference — measured side by side in the same run
/// (`Duration: 00:00:06.00` vs `elapsed=0:00:00.00`).
#[must_use]
pub(crate) fn elapsed_for_stats(seconds: f64) -> String {
    let (h, m, s, cs) = split_clock(seconds);
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

fn split_clock(seconds: f64) -> (i64, i64, i64, i64) {
    let total_cs = (seconds * 100.0).round();
    let total_cs = if total_cs.is_finite() {
        total_cs.max(0.0)
    } else {
        0.0
    };
    let cs = total_cs % 100.0;
    let total_s = (total_cs / 100.0).trunc();
    let s = total_s % 60.0;
    let total_m = (total_s / 60.0).trunc();
    let m = total_m % 60.0;
    let h = (total_m / 60.0).trunc();
    (h as i64, m as i64, s as i64, cs as i64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_metadata_key_field_pads_to_sixteen_but_never_truncates() {
        let mut out = Vec::new();
        metadata_block(
            &mut out,
            4,
            &[
                ("major_brand".to_owned(), "isom".to_owned()),
                ("minor_version".to_owned(), "512".to_owned()),
                (
                    "compatible_brands".to_owned(),
                    "isomiso2avc1mp41".to_owned(),
                ),
                ("encoder".to_owned(), "Lavf62.12.100".to_owned()),
            ],
        );
        assert_eq!(
            out,
            vec![
                "    Metadata:".to_owned(),
                "      major_brand     : isom".to_owned(),
                "      minor_version   : 512".to_owned(),
                "      compatible_brands: isomiso2avc1mp41".to_owned(),
                "      encoder         : Lavf62.12.100".to_owned(),
            ]
        );
    }

    #[test]
    fn language_is_excluded_from_the_metadata_block() {
        let mut out = Vec::new();
        metadata_block(&mut out, 2, &[("language".to_owned(), "eng".to_owned())]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn the_clock_matches_the_measured_examples() {
        assert_eq!(clock(6.0), "00:00:06.00");
        assert_eq!(clock(1.24), "00:00:01.24");
        assert_eq!(clock(0.0), "00:00:00.00");
    }

    #[test]
    fn rate_str_drops_the_decimals_only_when_they_are_zero() {
        assert_eq!(rate_str(Rational::new(25, 1)), "25");
        assert_eq!(rate_str(Rational::new(875, 38)), "23.03");
        assert_eq!(rate_str(Rational::new(30000, 1001)), "29.97");
        assert_eq!(rate_str(Rational::new(1000, 7)), "142.86");
    }

    #[test]
    fn tbn_gets_a_k_suffix_only_for_an_exact_multiple_of_a_thousand() {
        assert_eq!(tbn_str(Rational::new(1, 12800)), "12800");
        assert_eq!(tbn_str(Rational::new(1, 90000)), "90k");
        assert_eq!(tbn_str(Rational::new(1, 30000)), "30k");
    }

    #[test]
    fn fourcc_label_matches_both_measured_shapes() {
        assert_eq!(fourcc_label(*b"avc1"), "avc1");
        assert_eq!(fourcc_label([27, 0, 0, 0]), "[27][0][0][0]");
    }

    #[test]
    fn disposition_parens_are_space_separated_not_comma_joined() {
        assert_eq!(
            disposition_parens(Disposition::DEFAULT | Disposition::FORCED),
            " (default) (forced)"
        );
        assert_eq!(disposition_parens(Disposition::NONE), "");
    }
}
