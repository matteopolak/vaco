//! The `format`, `stream`, `packet` and `error` section emitters.
//!
//! Each walks its [`fields`](crate::fields) table in order and asks for a
//! value, so the emission order cannot drift from the table — the order *is*
//! the table. Deciding what to put in each slot is the only thing here.

use std::io::Write;

use vaco_codec_core::{CodecId, CodecParameters, Level, Profile};
use vaco_core::{MediaType, Rational, Result};
use vaco_format_core::{Chapter, Program, Stream, StreamSideData, display_rotation};
use vaco_packet::{Packet, PacketFlags};
use vaco_textformat::num;
use vaco_textformat::sections::SectionId;

use crate::emit::{Emit, Val};
use crate::fields::{self, Field, Scope};

/// The unknown-level sentinel. Raw video prints `level=-99`, as an integer —
/// the only field in the stream section whose absent form is not a string.
/// Observed:
///
/// ```sh
/// ffprobe -v quiet -of json -f rawvideo -video_size 32x24 -pixel_format gray \
///         -show_streams raw.yuv    # -> "level": -99
/// ```
pub const LEVEL_UNKNOWN: i64 = -99;

/// Everything the `format` section needs that is not on the demuxer.
#[derive(Clone, Copy, Debug)]
pub struct FormatInfo<'a> {
    /// Printed verbatim, and overridable with `-print_filename`.
    pub filename: &'a str,
    pub format_name: &'a str,
    pub format_long_name: &'a str,
    /// What the probe engine scored. `0` for a format forced with `-f`, which
    /// `vaco_format_core::Probe::force` already reports and the reference
    /// agrees with.
    pub probe_score: i64,
    /// File size in bytes, when the transport knows it.
    pub size: Option<u64>,
    pub nb_programs: usize,
    pub nb_stream_groups: usize,
}

/// The `format` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn format<W: Write>(
    e: &mut Emit<'_, W>,
    info: &FormatInfo<'_>,
    streams: &[Stream],
    duration: Option<f64>,
    metadata: &[(String, String)],
) -> Result<()> {
    let t = fields::FORMAT;
    e.tf().open(SectionId::FORMAT)?;

    let start = container_start_time(streams);
    // `format.bit_rate` is an **integer** number of bits per second, truncated,
    // and the truncation happens before formatting. Measured on two containers
    // whose duration is not a round number, which is the only way to see it —
    // an exact duration makes every rounding rule agree:
    //
    //   op_st.webm  20846 B / 2.008000 s  raw 83051.792829  ->  83051
    //   op_st.opus                       raw 79401.943683  ->  79401
    //
    // Both truncate; neither rounds. Passed on as an `f64` so that `-unit` and
    // `-prefix` still apply, but with no fractional part left to print.
    let bit_rate = match (info.size, duration) {
        (Some(size), Some(d)) if d > 0.0 => {
            let raw = size as f64 * 8.0 / d;
            raw.is_finite().then(|| raw.trunc())
        }
        _ => None,
    };

    e.field(t, "filename", &Val::s(info.filename))?;
    e.field(
        t,
        "nb_streams",
        &Val::I(i64::try_from(streams.len()).unwrap_or(i64::MAX)),
    )?;
    e.field(
        t,
        "nb_programs",
        &Val::I(i64::try_from(info.nb_programs).unwrap_or(i64::MAX)),
    )?;
    e.field(
        t,
        "nb_stream_groups",
        &Val::I(i64::try_from(info.nb_stream_groups).unwrap_or(i64::MAX)),
    )?;
    e.field(t, "format_name", &Val::s(info.format_name))?;
    e.field(t, "format_long_name", &Val::s(info.format_long_name))?;
    e.field(t, "start_time", &Val::opt_f(start))?;
    e.field(t, "duration", &Val::opt_f(duration))?;
    e.field(t, "size", &Val::opt_f(info.size.map(|s| s as f64)))?;
    e.field(t, "bit_rate", &Val::opt_f(bit_rate))?;
    e.field(t, "probe_score", &Val::I(info.probe_score))?;

    tags(e, SectionId::FORMAT_TAGS, metadata)?;
    e.tf().close()
}

/// The container's `start_time`: the earliest stream start, ignoring cover art.
///
/// A cover image has no position on any timeline, which is why
/// [`Stream::is_attached_pic`] exists; including it would drag the container
/// start to zero on any file with one.
fn container_start_time(streams: &[Stream]) -> Option<f64> {
    streams
        .iter()
        .filter(|s| !s.is_attached_pic())
        .filter_map(|s| {
            s.start_time_absolute()
                .map(vaco_core::Duration::as_secs_f64)
        })
        .reduce(f64::min)
}

/// One `stream` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn stream<W: Write>(e: &mut Emit<'_, W>, s: &Stream, show_ids: bool) -> Result<()> {
    e.tf().open(SectionId::STREAM)?;
    stream_fields(e, s, show_ids)?;
    disposition(e, SectionId::STREAM_DISPOSITION, s.disposition)?;
    tags(e, SectionId::STREAM_TAGS, &s.metadata)?;
    side_data(e, s)?;
    e.tf().close()
}

/// The `side_data_list` sub-section, emitted only when the stream carries side
/// data — the reference opens no list at all for a stream without any.
///
/// The section's *type* is the human name (`Display Matrix`), not a slug: the
/// `xml` writer prints it verbatim as `type="Display Matrix"` while `compact`
/// runs it through `sanitise_type` to get `side_datum/display_matrix:`. Both
/// were read off `ffprobe 8.1`, and passing the slug would have been right in
/// one writer and wrong in the other.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn side_data<W: Write>(e: &mut Emit<'_, W>, s: &Stream) -> Result<()> {
    if s.side_data.is_empty() {
        return Ok(());
    }
    e.tf().open(SectionId::STREAM_SIDE_DATA_LIST)?;
    for datum in &s.side_data {
        e.tf()
            .open_typed(SectionId::STREAM_SIDE_DATA, datum.name())?;
        e.tf().str("side_data_type", datum.name())?;
        match *datum {
            StreamSideData::DisplayMatrix(m) => {
                e.tf().str("displaymatrix", &display_matrix_text(&m))?;
                // Truncated toward zero, not rounded. Measured: an exact
                // -35.683 prints -35 and an exact 26.978 prints 26.
                e.tf()
                    .int("rotation", display_rotation(&m).trunc() as i64)?;
            }
        }
        e.tf().close()?;
    }
    e.tf().close()
}

/// The `displaymatrix` value: a leading newline, then one line per row.
///
/// **Measured byte for byte** through the `json` writer, which is the only one
/// that shows the value's own bytes rather than the writer's line breaks:
///
/// ```text
/// "\n00000000:            0       65536           0\n…\n"
/// ```
///
/// Each row is `%08x:` then three right-aligned 12-column integers with a
/// single space after the colon — 37 characters after the colon, not the 36 a
/// `": %11d %11d %11d"` reading would give. The `-2147483648` case is what
/// pins the width down, because it is the only value wide enough to collide
/// with a wrong one.
fn display_matrix_text(m: &[i32; 9]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\n");
    for row in 0..3usize {
        let at = |i: usize| m.get(row * 3 + i).copied().unwrap_or(0);
        // `write!` into a `String` is infallible; the `Result` is discarded
        // deliberately rather than unwrapped, which is denied here.
        let _ = writeln!(out, "{row:08x}: {:12}{:12}{:12}", at(0), at(1), at(2));
    }
    out
}

fn stream_fields<W: Write>(e: &mut Emit<'_, W>, s: &Stream, show_ids: bool) -> Result<()> {
    let p = &s.params;
    let media = s.media_type();
    for field in fields::STREAM {
        if !in_scope(field, media) {
            continue;
        }
        let mut val = stream_value(field, s, p, media);
        // `id` is printed only by a container that declares
        // `FormatFlags::SHOW_IDS`. Measured: the same H.264 track reports
        // `id=0x1` from MP4 and `id=N/A` from Matroska, and Matroska's
        // `TrackNumber` is every bit as real an identifier — the reference
        // simply does not print it. Suppressing it here rather than leaving
        // `Stream::id` unset keeps `-map 0:#1` working on Matroska, which is
        // the other thing the field is for.
        if field.name == "id" && !show_ids {
            val = Val::Absent;
        }
        e.put(Some(field), &val)?;
    }
    Ok(())
}

/// Whether a stream field applies to a stream of this media type.
fn in_scope(field: &Field, media: Option<MediaType>) -> bool {
    match field.scope {
        Scope::Always => true,
        Scope::Video => media == Some(MediaType::Video),
        Scope::Audio => media == Some(MediaType::Audio),
        Scope::VideoOrSubtitle => {
            matches!(media, Some(MediaType::Video | MediaType::Subtitle))
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per field; splitting it would hide the order this asserts"
)]
fn stream_value(field: &Field, s: &Stream, p: &CodecParameters, media: Option<MediaType>) -> Val {
    let video = p.video.as_ref();
    let audio = p.audio.as_ref();
    let tb = s.time_base;
    match field.name {
        "index" => Val::I(i64::from(s.index)),
        "codec_name" => Val::opt_s(p.codec_id.map(CodecId::name)),
        "codec_long_name" => Val::opt_s(p.codec_id.map(CodecId::long_name)),
        // `unknown` rather than absent: every stream prints a profile, and a
        // codec without profiles prints the word.
        "profile" => Val::s(p.profile.map_or("unknown", |x: Profile| x.name)),
        "codec_type" => Val::opt_s(media.map(MediaType::name)),
        "codec_tag_string" => Val::s(codec_tag_string(p.codec_tag)),
        "codec_tag" => Val::s(num::codec_tag(codec_tag_u32(p.codec_tag))),
        "mime_codec_string" => Val::opt_s(mime_codec_string(p)),
        "width" => Val::opt_i(video.map(|v| i64::from(v.width))),
        "height" => Val::opt_i(video.map(|v| i64::from(v.height))),
        "coded_width" => Val::opt_i(video.map(|v| i64::from(v.coded_width))),
        "coded_height" => Val::opt_i(video.map(|v| i64::from(v.coded_height))),
        "has_b_frames" => Val::opt_i(video.map(|v| i64::from(v.has_b_frames))),
        "sample_aspect_ratio" => match video.map(|v| v.sample_aspect_ratio) {
            Some(sar) if sar.num != 0 => Val::s(num::ratio(sar)),
            _ => Val::Absent,
        },
        "display_aspect_ratio" => match (video, video.map(|v| v.sample_aspect_ratio)) {
            (Some(v), Some(sar)) if sar.num != 0 => {
                Val::s(num::ratio(display_aspect(v.width, v.height, sar)))
            }
            _ => Val::Absent,
        },
        "pix_fmt" => Val::opt_s(video.and_then(|v| v.format).map(vaco_pixfmt::PixFmt::name)),
        "level" => Val::I(p.level.map_or(LEVEL_UNKNOWN, |Level(l)| i64::from(l))),
        "color_range" => colour(video.map(|v| v.color.range.name())),
        "color_space" => colour(video.map(|v| v.color.matrix.name())),
        "color_transfer" => colour(video.map(|v| v.color.transfer.name())),
        "color_primaries" => colour(video.map(|v| v.color.primaries.name())),
        "chroma_location" => colour(video.map(|v| v.color.chroma_location.name())),
        "field_order" => Val::s(field_order_name(video.map(|v| v.field_order))),
        // The H.264 decoder's two private options, both strings, both derived
        // from one number. `nal_length_size` is the container's length prefix
        // width and `is_avc` is "that width is non-zero"; measured, the same
        // content in MP4 reports `true`/`4` and in MPEG-TS reports `false`/`0`.
        // `None` for every other codec, which is what keeps the pair out of an
        // HEVC or AV1 stream's output.
        "is_avc" => Val::opt_s(
            video
                .and_then(|v| v.nal_length_size)
                .map(|n| if n > 0 { "true" } else { "false" }.to_owned()),
        ),
        "nal_length_size" => {
            Val::opt_s(video.and_then(|v| v.nal_length_size).map(|n| n.to_string()))
        }
        // The HEVC decoder's two, and they are **empty strings**, not absent:
        // `view_ids_available=""` is printed for a plain single-layer stream.
        // They list MV-HEVC layer ids, and this build parses no layer set, so
        // the empty list is also the correct value rather than a placeholder.
        "view_ids_available" | "view_pos_available" => {
            if p.codec_id == Some(CodecId::Hevc) {
                Val::s(String::new())
            } else {
                Val::Absent
            }
        }
        "sample_fmt" => Val::opt_s(
            audio
                .and_then(|a| a.format)
                .map(vaco_sampfmt::SampleFmt::name),
        ),
        // A string holding a plain number. Measured, not a slip.
        //
        // Always printed, never `N/A`: the field is only reached for an audio
        // stream, and the reference prints the raw value including zero. (Zero
        // itself was not reachable from any input that could be built, so the
        // *spelling* of a zero sample rate is unverified; printing `0` is the
        // only rendering consistent with every rate that could be observed.)
        "sample_rate" => Val::s(audio.map_or(0, |a| a.sample_rate).to_string()),
        "channels" => Val::I(
            audio
                .and_then(|a| a.layout.as_ref())
                .map_or(0, |l| i64::from(l.channels)),
        ),
        "channel_layout" => Val::opt_s(
            audio
                .and_then(|a| a.layout.as_ref())
                .and_then(vaco_chlayout::ChannelLayout::name),
        ),
        "bits_per_sample" => Val::opt_i(audio.map(|_| i64::from(bits_per_sample(p.codec_id)))),
        "initial_padding" => Val::opt_i(audio.map(|a| i64::from(a.initial_padding))),
        "id" => Val::opt_s(s.id.map(num::id)),
        // Two fields, two sources. They differ on a variable-rate file:
        // a 1/600-timescale MP4 whose `stts` holds mostly 60-tick deltas
        // reports `r_frame_rate=10/1` and `avg_frame_rate=300/29`.
        "r_frame_rate" => Val::s(num::rational(frame_rate(s.r_frame_rate, video))),
        "avg_frame_rate" => Val::s(num::rational(frame_rate(s.avg_frame_rate, video))),
        "time_base" => Val::s(num::rational(tb)),
        "start_pts" => Val::opt_i(s.start_time.ticks()),
        "start_time" => Val::opt_f(
            s.start_time_absolute()
                .map(vaco_core::Duration::as_secs_f64),
        ),
        // Straight off the field: the microsecond round-trip this used to go
        // through could not represent 25 500 ticks at 1/12800 at all.
        "duration_ts" => Val::opt_i(s.duration_ts),
        "duration" => Val::opt_f(s.duration().map(vaco_core::Duration::as_secs_f64)),
        "bit_rate" => Val::opt_f(p.bit_rate.map(|b| b as f64)),
        // `max_bit_rate` is not on the frozen `CodecParameters`, and
        // `nb_read_frames`/`nb_read_packets` need `-count_frames` /
        // `-count_packets`. All three fall through to the wildcard, which is
        // `Val::Absent` — the reference prints `N/A` for them too until
        // something fills them.
        "bits_per_raw_sample" => Val::opt_s(bits_per_raw_sample(p).map(|b| b.to_string())),
        "nb_frames" => Val::opt_s(s.frame_count.map(|n| n.to_string())),
        "extradata_size" => Val::opt_i(
            p.extradata
                .as_ref()
                .map(|x| i64::try_from(x.len()).unwrap_or(i64::MAX)),
        ),
        _ => Val::Absent,
    }
}

/// The colour fields are never truly absent: the model always has a value, and
/// `Unspecified` spells itself (`unknown`, or `unspecified` for chroma). They
/// still go through the *optional* path, which is what makes `json` omit them
/// and `flat` print the word — so an unspecified value becomes [`Val::Absent`]
/// and [`crate::fields::Absent::Word`] supplies the spelling.
fn colour(name: Option<&'static str>) -> Val {
    match name {
        Some("unknown" | "unspecified") | None => Val::Absent,
        Some(n) => Val::s(n),
    }
}

/// The stream's own rate, falling back to the codec parameters.
///
/// The fallback exists because `CodecParameters::video.frame_rate` is filled by
/// the bitstream parsers as well as by the container, and a rate a parser found
/// is still a rate. `Rational::UNDEFINED` prints `0/0`, which is what the
/// reference prints for a stream with no frame rate — including every audio
/// stream.
fn frame_rate(stated: Rational, video: Option<&vaco_codec_core::VideoParameters>) -> Rational {
    if stated.den != 0 && !stated.is_zero() {
        return stated;
    }
    match video.map(|v| v.frame_rate) {
        Some(r) if r.den != 0 => r,
        _ => Rational::UNDEFINED,
    }
}

/// `display_aspect_ratio` = `width * sar : height`, reduced.
fn display_aspect(width: u32, height: u32, sar: Rational) -> Rational {
    let num = i64::from(width) * i64::from(sar.num);
    let den = i64::from(height) * i64::from(sar.den);
    Rational::new(
        num.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
        den.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
    )
    .reduced()
}

fn field_order_name(order: Option<vaco_codec_core::FieldOrder>) -> &'static str {
    use vaco_codec_core::FieldOrder as F;
    match order {
        Some(F::Progressive) => "progressive",
        Some(F::TopFirst) => "tt",
        Some(F::BottomFirst) => "bb",
        Some(F::TopCodedFirst) => "tb",
        Some(F::BottomCodedFirst) => "bt",
        Some(F::Unknown) | None => "unknown",
    }
}

/// Bits per *decoded* sample, which the reference reports as 0 for every
/// compressed audio codec and as the container's word size for PCM.
///
/// `CodecParameters` has no field for it and PCM's variants are not modelled
/// separately yet, so this reports 0 — matching every non-PCM observation.
const fn bits_per_sample(codec: Option<CodecId>) -> u32 {
    let _ = codec;
    0
}

/// `bits_per_raw_sample`, which is a **codec** property and not a container
/// one — and the two get confused in exactly one direction.
///
/// Video first, because that is where the reference actually prints it and the
/// model could not express it until `VideoParameters` grew the field:
///
/// ```text
/// av.mp4   h264 yuv420p     -> bits_per_raw_sample="8"    aac -> "N/A"
/// 10-bit   h264 yuv420p10le -> bits_per_raw_sample="10"
/// hevc.mp4 hevc yuv420p     -> bits_per_raw_sample="N/A"
/// av1.mp4  av1  yuv420p     -> bits_per_raw_sample="N/A"
/// ```
///
/// # The float suppression, and the crate-level defect behind it
///
/// `vaco-demux-mp4` and `vaco-demux-matroska` both fill
/// `AudioParameters::bits_per_raw_sample` from the container's own sample
/// depth — MP4's `stsd` sample entry `sample_size`, Matroska's `BitDepth` — so
/// an AAC track reports 16 and an Opus track reports 32. The reference reports
/// `N/A` for both. That number is not wrong, it is in the **wrong field**:
/// probed on a WAV, `pcm_s16le` prints `bits_per_sample=16` and
/// `bits_per_raw_sample="N/A"`, so the container's depth is
/// `bits_per_coded_sample`, which `CodecParameters` has nowhere to put.
///
/// Until it does, this suppresses the value for a stream whose decoded sample
/// format is **floating point**. A raw-sample bit count is meaningless for a
/// float decoder, and every float-output stream measured — AAC in MP4, MOV,
/// Matroska and `MPEG-TS`, Opus in Matroska and `WebM` — reports `N/A`. Integer
/// audio is untouched: `pcm_s24le` in MOV reports `24` and must keep doing so.
/// Reported upstream rather than being called a fix; see the doc file.
fn bits_per_raw_sample(p: &CodecParameters) -> Option<u8> {
    if let Some(v) = p.video.as_ref() {
        return v.bits_per_raw_sample;
    }
    let audio = p.audio.as_ref()?;
    let bits = audio.bits_per_raw_sample?;
    if audio.format.is_some_and(vaco_sampfmt::SampleFmt::is_float) {
        return None;
    }
    Some(bits)
}

/// `codec_tag_string`: printable ASCII kept, everything else as `[n]`.
///
/// Observed both ways in one session: `avc1` for MP4's four-character code and
/// `[27][0][0][0]` for MPEG-TS's stream type 27, which is the same four bytes
/// rendered by the same rule.
fn codec_tag_string(tag: Option<[u8; 4]>) -> String {
    let Some(tag) = tag else {
        return "[0][0][0][0]".to_owned();
    };
    let mut out = String::new();
    for b in tag {
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

fn codec_tag_u32(tag: Option<[u8; 4]>) -> u32 {
    tag.map_or(0, u32::from_le_bytes)
}

/// The RFC 6381 codecs parameter, as `mime_codec_string`.
///
/// Derived from the profile and level rather than from `extradata`, which is
/// what the reference does: an MPEG-TS stream carrying Annex B (no `avcC` at
/// all) still reports `avc1.64000a`, identical to the same content in MP4.
/// Probed:
///
/// ```sh
/// ffprobe -v quiet -of json -show_entries stream=mime_codec_string,profile,level col.mp4
/// #   High / 10 -> avc1.64000a
/// ffprobe -v quiet -of json -show_entries stream=mime_codec_string,profile,level av.mp4
/// #   High / 13 -> avc1.64000d,  LC -> mp4a.40.2
/// ```
///
/// The middle byte is the H.264 constraint-flag set, which
/// `CodecParameters` does not carry; `00` matched every sample observed and is
/// what a `High` profile stream has. Recorded as a known gap.
fn mime_codec_string(p: &CodecParameters) -> Option<String> {
    let profile = p.profile.map(|x: Profile| x.value);
    match p.codec_id? {
        CodecId::H264 => {
            let Level(level) = p.level?;
            Some(format!(
                "avc1.{:02x}00{:02x}",
                profile? & 0xff,
                level & 0xff
            ))
        }
        // `av01.<profile>.<level><tier>.<depth>`, RFC 6381 / AV1 ISOBMFF §5.
        // Probed at two depths on the same encoder:
        //
        //   profile 0, level 0, yuv420p     -> av01.0.00M.08
        //   profile 0, level 0, yuv420p10le -> av01.0.00M.10
        //
        // The tier is `M` in both. `CodecParameters` does not carry
        // `seq_tier`, and no `H`-tier sample could be produced with the
        // encoders available, so `M` is what is emitted and the gap is
        // recorded rather than guessed at.
        CodecId::Av1 => {
            let Level(level) = p.level.unwrap_or(Level(0));
            Some(format!(
                "av01.{}.{:02}M.{:02}",
                profile.unwrap_or(0).clamp(0, 9),
                level.clamp(0, 31),
                p.video
                    .as_ref()
                    .and_then(|v| v.format)
                    .map_or(8, vaco_pixfmt::PixFmt::max_depth),
            ))
        }
        // `mp4a.40.<audioObjectType>`; the object type is the profile plus one,
        // so AAC-LC (profile 1) prints `mp4a.40.2`.
        CodecId::Aac | CodecId::AacLatm => Some(format!(
            "mp4a.40.{}",
            profile.unwrap_or(1).saturating_add(1)
        )),
        CodecId::Opus => Some("opus".to_owned()),
        CodecId::Flac => Some("flac".to_owned()),
        CodecId::Vp8 => Some("vp8".to_owned()),
        // `vp09.<profile>.<level>.<depth>` — probed as `vp09.00.10.08`. Left
        // as the bare four-character form because this build has no VP9 parser,
        // so `profile` and `level` are both unknown and a fabricated
        // `vp09.00.10.08` would be right by coincidence on one file and wrong
        // on the next. Closes when a `vaco-parse-vp9` exists.
        CodecId::Vp9 => Some("vp09".to_owned()),
        CodecId::Mp3 => Some("mp4a.40.34".to_owned()),
        // HEVC falls through here deliberately. Measured on an `hvc1`-tagged
        // MP4: the reference prints no `mime_codec_string` at all for HEVC, in
        // any writer, at any `-show_optional_fields` setting. The
        // four-character code alone is not an RFC 6381 codecs parameter and the
        // reference does not pretend it is; we printed `hvc1` and were wrong.
        _ => None,
    }
}

/// The `disposition` sub-section: one integer field per flag, always all of
/// them, in the reference's bit order.
///
/// The name list comes from `vaco_cli_core::Disposition`, which carries all
/// **19** flags. `vaco_format_core::Disposition` carries 15 and is missing
/// `clean_effects`, `timed_thumbnails`, `non_diegetic` and `multilayer` — so
/// printing from the container model would produce a 15-field section where the
/// reference prints 19. The four missing flags are always zero here; see the
/// doc file, this is a reported gap in `vaco-format-core`.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn disposition<W: Write>(
    e: &mut Emit<'_, W>,
    section: SectionId,
    d: vaco_format_core::Disposition,
) -> Result<()> {
    e.tf().open(section)?;
    for &(_, name) in vaco_cli_core::Disposition::ALL {
        let set = vaco_format_core::Disposition::from_cli_name(name).is_some_and(|f| d.contains(f));
        e.tf().int(name, i64::from(set))?;
    }
    e.tf().close()
}

/// A `tags` sub-section, emitted only when there is at least one tag.
///
/// The reference opens no section at all for empty metadata — `[FORMAT]` on a
/// raw file has no `TAG:` lines and `json` has no `"tags"` key. Observed.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn tags<W: Write>(
    e: &mut Emit<'_, W>,
    section: SectionId,
    metadata: &[(String, String)],
) -> Result<()> {
    if metadata.is_empty() {
        return Ok(());
    }
    e.tf().open(section)?;
    for (k, v) in metadata {
        e.tf().tag(k, v)?;
    }
    e.tf().close()
}

/// One `packet` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn packet<W: Write>(e: &mut Emit<'_, W>, pkt: &Packet, stream: Option<&Stream>) -> Result<()> {
    let t = fields::PACKET;
    let tb = stream.map_or(Rational::MICROSECONDS, |s| s.time_base);
    let secs = |ts: Option<i64>| -> Option<f64> {
        let ts = ts?;
        if tb.den == 0 {
            return None;
        }
        Some(ts as f64 * f64::from(tb.num) / f64::from(tb.den))
    };

    e.tf().open(SectionId::PACKET)?;
    e.field(
        t,
        "codec_type",
        &Val::opt_s(stream.and_then(Stream::media_type).map(MediaType::name)),
    )?;
    e.field(t, "stream_index", &Val::I(i64::from(pkt.stream_index)))?;
    e.field(t, "pts", &Val::opt_i(pkt.pts.ticks()))?;
    e.field(t, "pts_time", &Val::opt_f(secs(pkt.pts.ticks())))?;
    e.field(t, "dts", &Val::opt_i(pkt.dts.ticks()))?;
    e.field(t, "dts_time", &Val::opt_f(secs(pkt.dts.ticks())))?;
    // `duration` counts ticks of the stream's time base, `duration_time` is the
    // same quantity in seconds. The model carries a `Duration` in microseconds,
    // so the tick count is derived rather than stored.
    let ticks = pkt.duration.to_ticks(tb).filter(|t| *t != 0);
    e.field(t, "duration", &Val::opt_i(ticks))?;
    e.field(
        t,
        "duration_time",
        &Val::opt_f(ticks.and_then(|_| {
            let secs = pkt.duration.as_secs_f64();
            (secs != 0.0).then_some(secs)
        })),
    )?;
    e.field(t, "size", &Val::F(pkt.payload().len() as f64))?;
    e.field(t, "pos", &Val::opt_f(pkt.pos.map(|p| p as f64)))?;
    e.field(
        t,
        "flags",
        &Val::s(num::packet_flags(
            pkt.flags.contains(PacketFlags::KEY),
            pkt.flags.contains(PacketFlags::DISCARD),
            pkt.flags.contains(PacketFlags::CORRUPT),
        )),
    )?;
    e.tf().close()
}

/// The `error` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn error<W: Write>(e: &mut Emit<'_, W>, code: i64, text: &str) -> Result<()> {
    let t = fields::ERROR;
    e.tf().open(SectionId::ERROR)?;
    e.field(t, "code", &Val::I(code))?;
    e.field(t, "string", &Val::s(text))?;
    e.tf().close()
}

/// One `chapter` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn chapter<W: Write>(e: &mut Emit<'_, W>, c: &Chapter) -> Result<()> {
    let tb = c.time_base;
    let secs = |ts: Option<i64>| -> Option<f64> {
        let ts = ts?;
        if tb.den == 0 {
            return None;
        }
        Some(ts as f64 * f64::from(tb.num) / f64::from(tb.den))
    };
    e.tf().open(SectionId::CHAPTER)?;
    e.tf().str("id", &c.id.to_string())?;
    e.tf().str("time_base", &num::rational(tb))?;
    e.tf().ts("start", c.start.ticks())?;
    e.tf().duration("start_time", secs(c.start.ticks()))?;
    e.tf().ts("end", c.end.ticks())?;
    e.tf().duration("end_time", secs(c.end.ticks()))?;
    tags(e, SectionId::CHAPTER_TAGS, &c.metadata)?;
    e.tf().close()
}

/// One `program` section, with its member streams nested inside it.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn program<W: Write>(
    e: &mut Emit<'_, W>,
    p: &Program,
    streams: &[Stream],
    show_ids: bool,
) -> Result<()> {
    let members = p
        .stream_indices
        .iter()
        .filter(|i| streams.iter().any(|s| s.index == **i))
        .count();
    e.tf().open(SectionId::PROGRAM)?;
    // The five fields the reference prints, in the order it prints them.
    // Measured with `-of flat -show_optional_fields always -show_programs`,
    // which shows every field a section defines including the unavailable
    // ones: `program_id`, `program_num`, `nb_streams`, `pmt_pid`, `pcr_pid`,
    // then the tags. There is no `pmt_version` field, no `start_time` and no
    // `end_time`, which is where plan 18 §1.1 is wrong.
    e.tf().int("program_id", p.id)?;
    e.tf().int_opt("program_num", p.program_num)?;
    e.tf()
        .int("nb_streams", i64::try_from(members).unwrap_or(i64::MAX))?;
    e.tf().int_opt("pmt_pid", p.pmt_pid.map(i64::from))?;
    e.tf().int_opt("pcr_pid", p.pcr_pid.map(i64::from))?;
    tags(e, SectionId::PROGRAM_TAGS, &p.metadata)?;
    e.tf().open(SectionId::PROGRAM_STREAMS)?;
    for index in &p.stream_indices {
        if let Some(s) = streams.iter().find(|s| s.index == *index) {
            e.tf().open(SectionId::PROGRAM_STREAM)?;
            stream_fields(e, s, show_ids)?;
            disposition(e, SectionId::PROGRAM_STREAM_DISPOSITION, s.disposition)?;
            tags(e, SectionId::PROGRAM_STREAM_TAGS, &s.metadata)?;
            e.tf().close()?;
        }
    }
    e.tf().close()?;
    e.tf().close()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn codec_tag_string_renders_both_observed_shapes() {
        assert_eq!(codec_tag_string(Some(*b"avc1")), "avc1");
        assert_eq!(codec_tag_string(Some([27, 0, 0, 0])), "[27][0][0][0]");
        assert_eq!(codec_tag_string(Some([0; 4])), "[0][0][0][0]");
        assert_eq!(codec_tag_string(None), "[0][0][0][0]");
    }

    #[test]
    fn codec_tag_is_little_endian() {
        // ffprobe prints codec_tag_string=mp4a with codec_tag=0x6134706d.
        assert_eq!(num::codec_tag(codec_tag_u32(Some(*b"mp4a"))), "0x6134706d");
        assert_eq!(num::codec_tag(codec_tag_u32(Some(*b"avc1"))), "0x31637661");
    }

    /// `mime_codec_string` per codec, each form probed on a real file. The
    /// three video codecs answer three different shapes and one of them answers
    /// nothing, which is why this is a table and not a rule.
    #[test]
    fn mime_codec_string_is_per_codec_and_hevc_has_none() {
        // ffprobe -show_entries stream=mime_codec_string hevc.mp4  ->  absent
        let hevc = CodecParameters::video().with_codec(CodecId::Hevc);
        assert_eq!(mime_codec_string(&hevc), None);

        // av1.mp4        profile 0, level 0, yuv420p     -> av01.0.00M.08
        // 10-bit av1     profile 0, level 0, yuv420p10le -> av01.0.00M.10
        let mut av1 = CodecParameters::video().with_codec(CodecId::Av1);
        av1.profile = Some(Profile {
            value: 0,
            name: "Main",
        });
        av1.level = Some(Level(0));
        if let Some(v) = av1.video.as_mut() {
            v.format = vaco_pixfmt::PixFmt::from_name("yuv420p").ok();
        }
        assert_eq!(mime_codec_string(&av1).as_deref(), Some("av01.0.00M.08"));
        if let Some(v) = av1.video.as_mut() {
            v.format = vaco_pixfmt::PixFmt::from_name("yuv420p10le").ok();
        }
        assert_eq!(mime_codec_string(&av1).as_deref(), Some("av01.0.00M.10"));
    }

    /// `is_avc` and `nal_length_size` are H.264 decoder *private* options, and
    /// they are container facts rather than bitstream ones. Probed on the same
    /// content in two containers:
    ///
    /// ```text
    /// av.mp4  ->  is_avc="true"   nal_length_size="4"
    /// ts.ts   ->  is_avc="false"  nal_length_size="0"
    /// hevc    ->  neither field printed at all
    /// ```
    #[test]
    fn the_h264_private_options_distinguish_zero_from_absent() {
        fn field(name: &str) -> Field {
            crate::fields::STREAM
                .iter()
                .find(|f| f.name == name)
                .copied()
                .expect("in the table")
        }
        let stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let render = |size: Option<u8>, name: &str| {
            let mut p = CodecParameters::video().with_codec(CodecId::H264);
            if let Some(v) = p.video.as_mut() {
                v.nal_length_size = size;
            }
            stream_value(&field(name), &stream, &p, Some(MediaType::Video))
        };
        let text = |v: Val| match v {
            Val::S(s) => Some(s),
            _ => None,
        };
        assert_eq!(text(render(Some(4), "is_avc")).as_deref(), Some("true"));
        assert_eq!(
            text(render(Some(4), "nal_length_size")).as_deref(),
            Some("4")
        );
        // Annex B: a *value*, not an absence.
        assert_eq!(text(render(Some(0), "is_avc")).as_deref(), Some("false"));
        assert_eq!(
            text(render(Some(0), "nal_length_size")).as_deref(),
            Some("0")
        );
        // Another codec: absent, and `Absent::Omit` means nothing is printed.
        assert!(matches!(render(None, "is_avc"), Val::Absent));
        assert!(matches!(render(None, "nal_length_size"), Val::Absent));
        assert_eq!(field("is_avc").absent, crate::fields::Absent::Omit);
        assert_eq!(field("nal_length_size").absent, crate::fields::Absent::Omit);
    }

    /// `bits_per_raw_sample` is a codec property, and the container's own
    /// sample depth is a *different* field the model cannot yet hold. See the
    /// function's own note.
    #[test]
    fn bits_per_raw_sample_prefers_video_and_suppresses_float_audio() {
        let mut video = CodecParameters::video().with_codec(CodecId::H264);
        if let Some(v) = video.video.as_mut() {
            v.bits_per_raw_sample = Some(8);
        }
        assert_eq!(bits_per_raw_sample(&video), Some(8));

        // AAC: the demuxer supplies the container's 16, the reference prints
        // `N/A`, and the decoder's output format is float.
        let mut aac = CodecParameters::audio().with_codec(CodecId::Aac);
        if let Some(a) = aac.audio.as_mut() {
            a.bits_per_raw_sample = Some(16);
            a.format = Some(vaco_sampfmt::SampleFmt::F32P);
        }
        assert_eq!(bits_per_raw_sample(&aac), None);

        // Integer audio keeps its value: `pcm_s24le` in MOV reports 24.
        let mut pcm = CodecParameters::audio().with_codec(CodecId::Pcm);
        if let Some(a) = pcm.audio.as_mut() {
            a.bits_per_raw_sample = Some(24);
            a.format = Some(vaco_sampfmt::SampleFmt::S32);
        }
        assert_eq!(bits_per_raw_sample(&pcm), Some(24));
    }

    #[test]
    fn mime_codec_string_matches_the_observed_samples() {
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        p.profile = Some(Profile {
            value: 100,
            name: "High",
        });
        p.level = Some(Level(13));
        assert_eq!(mime_codec_string(&p).as_deref(), Some("avc1.64000d"));
        p.level = Some(Level(10));
        assert_eq!(mime_codec_string(&p).as_deref(), Some("avc1.64000a"));

        let mut a = CodecParameters::audio().with_codec(CodecId::Aac);
        a.profile = Some(Profile {
            value: 1,
            name: "LC",
        });
        assert_eq!(mime_codec_string(&a).as_deref(), Some("mp4a.40.2"));

        // No profile and no level means no string at all, not a malformed one.
        let bare = CodecParameters::video().with_codec(CodecId::H264);
        assert_eq!(mime_codec_string(&bare), None);
    }

    #[test]
    fn display_aspect_is_reduced() {
        assert_eq!(display_aspect(320, 240, Rational::ONE), Rational::new(4, 3));
        assert_eq!(display_aspect(63, 48, Rational::ONE), Rational::new(21, 16));
        // A 16:9 1920x1080 square-pixel frame.
        assert_eq!(
            display_aspect(1920, 1080, Rational::ONE),
            Rational::new(16, 9)
        );
    }

    #[test]
    fn colour_maps_the_unspecified_spellings_to_absent() {
        assert!(matches!(colour(Some("unknown")), Val::Absent));
        assert!(matches!(colour(Some("unspecified")), Val::Absent));
        assert!(matches!(colour(None), Val::Absent));
        assert!(matches!(colour(Some("bt709")), Val::S(_)));
    }

    #[test]
    fn frame_rate_of_a_non_video_stream_is_zero_over_zero() {
        assert_eq!(num::rational(frame_rate(Rational::UNDEFINED, None)), "0/0");
    }

    /// `format.bit_rate` truncates. Regression: it was emitted as a bare
    /// `size * 8 / duration`, which printed `bit_rate=83051.792829` where the
    /// reference prints `83051`. Invisible on every sample whose duration is a
    /// round number, which is what the corpus had until a webm was added.
    #[test]
    fn format_bit_rate_truncates_to_whole_bits_per_second() {
        let info = |size| FormatInfo {
            filename: "x",
            format_name: "f",
            format_long_name: "F",
            probe_score: 100,
            size: Some(size),
            nb_programs: 0,
            nb_stream_groups: 0,
        };
        let render = |size: u64, duration: f64| -> String {
            let w = vaco_textformat::writers::make("flat").expect("writer");
            let mut tf = vaco_textformat::TextFormat::new(
                w,
                Vec::new(),
                vaco_textformat::FormatOpts::default(),
            );
            tf.open(SectionId::ROOT).expect("root");
            {
                let mut e = Emit::new(&mut tf, vaco_textformat::OptionalFields::Auto);
                format(&mut e, &info(size), &[], Some(duration), &[]).expect("format");
            }
            tf.close().expect("root");
            String::from_utf8(tf.finish().expect("finish")).expect("utf8")
        };

        // ffprobe -v quiet -of flat \
        //   -show_entries format=size,duration,bit_rate op_st.webm
        //   -> size="20846" duration="2.008000" bit_rate="83051"
        assert!(
            render(20_846, 2.008).contains("bit_rate=\"83051\""),
            "{}",
            render(20_846, 2.008)
        );
        // An exact duration must be unaffected: av.mp4, 88307 B over 2 s.
        assert!(render(88_307, 2.0).contains("bit_rate=\"353228\""));
        // A zero or absent duration yields no rate at all, not a division.
        assert!(render(1, 0.0).contains("bit_rate=\"N/A\""));
    }

    #[test]
    fn container_start_time_ignores_cover_art() {
        use vaco_core::Timestamp;
        let tb = Rational::new(1, 1000);
        let mut art = Stream::new(0, MediaType::Video, tb);
        art.start_time = Timestamp::new(0);
        art.disposition = vaco_format_core::Disposition::ATTACHED_PIC;
        let mut audio = Stream::new(1, MediaType::Audio, tb);
        audio.start_time = Timestamp::new(5000);
        audio.duration_ts = Some(1_000_000);

        assert_eq!(container_start_time(&[art, audio]), Some(5.0));
        assert_eq!(container_start_time(&[]), None);
    }
}
