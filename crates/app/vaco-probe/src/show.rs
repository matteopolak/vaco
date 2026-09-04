//! The `format`, `stream`, `packet` and `error` section emitters.
//!
//! Each walks its [`fields`](crate::fields) table in order and asks for a
//! value, so the emission order cannot drift from the table — the order *is*
//! the table. Deciding what to put in each slot is the only thing here.

use std::io::Write;

use vaco_codec_core::{CodecId, CodecParameters, Level, Profile};
use vaco_core::{MediaType, Rational, Result};
use vaco_format_core::{
    Chapter, Program, Stream, StreamGroup, StreamGroupKind, StreamSideData, display_rotation,
};
use vaco_packet::{Packet, PacketFlags, PacketSideData};
use vaco_textformat::num;
use vaco_textformat::sections::SectionId;

use crate::dump::{DumpFormat, HashAlg};
use crate::emit::{Emit, Val};
use crate::fields::{self, Field, Scope};

/// What `-count_packets` and `-count_frames` filled in for one stream.
///
/// `nb_read_packets` and `nb_read_frames` are the only two stream fields that
/// cannot be answered from the header — they are the result of having read the
/// file, and they are **bounded by `-read_intervals` and `-select_streams`**.
/// Measured on a three-packet interval over a two-stream MP4: the counts are 2
/// and 1, not 50 and 88, so the counter tracks what was shown, not what exists.
#[derive(Clone, Copy, Default, Debug)]
pub struct Counts {
    pub read_packets: Option<u64>,
    pub read_frames: Option<u64>,
}

impl Counts {
    /// Neither counter was requested.
    pub const NONE: Self = Self {
        read_packets: None,
        read_frames: None,
    };
}

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
pub fn stream<W: Write>(
    e: &mut Emit<'_, W>,
    s: &Stream,
    show_ids: bool,
    counts: Counts,
) -> Result<()> {
    e.tf().open(SectionId::STREAM)?;
    stream_fields(e, s, show_ids, counts)?;
    disposition(e, SectionId::STREAM_DISPOSITION, s.disposition)?;
    tags(e, SectionId::STREAM_TAGS, &stream_visible_metadata(s))?;
    side_data(e, s)?;
    e.tf().close()
}

/// `s.metadata` minus keys already surfaced as dedicated `[STREAM]` fields
/// (`ts_id` and `ts_packetsize`).
///
/// `Stream::metadata` is the one channel this crate has for a demuxer to hand
/// back an out-of-band fact, and `vaco-demux-mpegts` uses it for two purposes
/// that must render completely differently: a real container tag (the SDT's
/// service name, say) is genuinely user-visible metadata and belongs in the
/// `tags` sub-section as `TAG:`, while `ts_id`/`ts_packetsize` are dedicated
/// fields the reference prints inline, never as a `TAG:`. Filtering by key
/// here — rather than a second field on `Stream` — keeps that distinction
/// entirely inside the two crates that already agree on the two names,
/// without widening `vaco_format_core::Stream`'s public shape for it.
fn stream_visible_metadata(s: &Stream) -> Vec<(String, String)> {
    s.metadata
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "ts_id" | "ts_packetsize"))
        .cloned()
        .collect()
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
        e.str("side_data_type", datum.name())?;
        match *datum {
            StreamSideData::DisplayMatrix(m) => {
                e.str("displaymatrix", &display_matrix_text(&m))?;
                // Truncated toward zero, not rounded. Measured: an exact
                // -35.683 prints -35 and an exact 26.978 prints 26.
                e.int("rotation", display_rotation(&m).trunc() as i64)?;
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

fn stream_fields<W: Write>(
    e: &mut Emit<'_, W>,
    s: &Stream,
    show_ids: bool,
    counts: Counts,
) -> Result<()> {
    let p = &s.params;
    let media = s.media_type();
    for field in fields::STREAM {
        if !in_scope(field, media) {
            continue;
        }
        let mut val = stream_value(field, s, p, media, counts, e.is_bitexact());
        // `id` is printed only by a container that declares
        // `FormatFlags::SHOW_IDS`. Measured: the same H.264 track reports
        // `id=0x1` from MP4 and `id=N/A` from Matroska, and Matroska's
        // `TrackNumber` is every bit as real an identifier — the reference
        // simply does not print it. Suppressing it here rather than leaving
        // `Stream::id` unset preserves Matroska stream-id mapping.
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
fn stream_value(
    field: &Field,
    s: &Stream,
    p: &CodecParameters,
    media: Option<MediaType>,
    counts: Counts,
    bitexact: bool,
) -> Val {
    let video = p.video.as_ref();
    let audio = p.audio.as_ref();
    let tb = s.time_base;
    match field.name {
        "index" => Val::I(i64::from(s.index)),
        "codec_name" => Val::opt_s(p.codec_id.map(CodecId::name)),
        "codec_long_name" => Val::opt_s(p.codec_id.map(CodecId::long_name)),
        // Absent, not the literal word. `Absent::Word("unknown")` in the field
        // table already renders `profile=unknown` for the writers that show
        // optional fields; handing it a *present* `"unknown"` here made `json`
        // and `xml` print a key the reference omits. Measured on FLAC:
        //
        //   ffprobe -v quiet -of json    -show_entries stream=profile f.flac  # no key
        //   ffprobe -v quiet -of default -show_entries stream=profile f.flac  # profile=unknown
        // `-bitexact` swaps the library name for the raw numeric value —
        // measured on H.264 (`High` -> `100`), AAC (`LC` -> `1`), VP9
        // (`Profile 0` -> `0`) and AV1 (`Main` -> `0`); `level` is unaffected,
        // it is already numeric in both modes. A profile with no name at all
        // (VP8, or an H.264 `profile_idc` the standard never named) prints
        // the number in *both* modes — `x.name` is `""` for those, per
        // `vaco_codec_core::Profile`'s own convention, so the empty-name case
        // takes the same branch as bitexact rather than printing `""`.
        "profile" => Val::opt_s(p.profile.map(|x: Profile| {
            if bitexact || x.name.is_empty() {
                x.value.to_string()
            } else {
                x.name.to_string()
            }
        })),
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
        // Measured (`ffprobe 8.1`): a stream with no known field order omits
        // `field_order` under the default `-show_optional_fields auto` in
        // `json`/`xml` (it carries `WriterFlags::SUPPRESS_OPTIONAL`) and
        // prints the placeholder `unknown` in the others — the same
        // optional-with-a-word-placeholder shape as `color_range` and
        // friends via `colour()`, not a field that is always present.
        "field_order" => match field_order_name(video.map(|v| v.field_order)) {
            "unknown" => Val::Absent,
            n => Val::s(n),
        },
        // The H.264 decoder's two private options, both strings, both derived
        // from one number. `nal_length_size` is the container's length prefix
        // width and `is_avc` is "that width is non-zero"; measured, the same
        // content in MP4 reports `true`/`4` and in MPEG-TS reports `false`/`0`.
        //
        // Gated on `codec_id == H264` explicitly, not merely on
        // `nal_length_size.is_some()`: HEVC's `hvcC` populates the same
        // `VideoParameters.nal_length_size` field so `vaco-mux-raw`/
        // `vaco-mux-mpegts` can decide whether a copied HEVC stream needs
        // Annex-B conversion — but `is_avc`/`nal_length_size` are AVC-
        // only options (`ffmpeg -h decoder=hevc` has neither), and measured
        // directly (`ffprobe -bitexact -show_streams` on an `hvc1`/MP4 HEVC
        // stream) the reference prints neither field for HEVC. AV1 and every
        // other codec never populate the field at all, so this gate only
        // changes HEVC's behaviour.
        "is_avc" => Val::opt_s(
            video
                .filter(|_| p.codec_id == Some(CodecId::H264))
                .and_then(|v| v.nal_length_size)
                .map(|n| if n > 0 { "true" } else { "false" }.to_owned()),
        ),
        "nal_length_size" => Val::opt_s(
            video
                .filter(|_| p.codec_id == Some(CodecId::H264))
                .and_then(|v| v.nal_length_size)
                .map(|n| n.to_string()),
        ),
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
        // MPEG-4 Part 2's own pair, gated the same way `is_avc`/
        // `nal_length_size` are gated on H.264 above: `quarter_sample`/
        // `divx_packed` are `VideoParameters` fields no other codec's
        // parser ever sets, but the explicit `codec_id` check keeps the
        // *rendering* rule stated once, matching the file's existing
        // convention rather than relying on every parser to leave the
        // field `None`.
        "quarter_sample" | "divx_packed" => Val::opt_s(
            video
                .filter(|_| p.codec_id == Some(CodecId::Mpeg4))
                .and_then(|v| {
                    if field.name == "quarter_sample" {
                        v.quarter_sample
                    } else {
                        v.divx_packed
                    }
                })
                .map(|b| if b { "true" } else { "false" }.to_owned()),
        ),
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
        // `vaco-demux-mpegts` is the one demuxer that sets these
        // two, via `Stream::metadata` — see `stream`'s `tags` call below for
        // the other half (they must not *also* print as `TAG:`). Absent on
        // every other container, since nothing else sets the key, which is
        // exactly `Absent::Omit`'s "no placeholder" behaviour.
        "ts_id" => Val::opt_s(s.metadata_get("ts_id")),
        "ts_packetsize" => Val::opt_s(s.metadata_get("ts_packetsize")),
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
        // `max_bit_rate` is not on the frozen `CodecParameters`, so it falls
        // through to the wildcard and prints `N/A`, as the reference does
        // until something fills it.
        "bits_per_raw_sample" => Val::opt_s(bits_per_raw_sample(p).map(|b| b.to_string())),
        "nb_frames" => Val::opt_s(s.frame_count.map(|n| n.to_string())),
        // Strings, not integers, next to `nb_frames` which is also a string —
        // see the module note. Absent unless the matching count flag was
        // given, which is the whole difference between `-count_packets` and
        // reading the header.
        "nb_read_packets" => Val::opt_s(counts.read_packets.map(|n| n.to_string())),
        "nb_read_frames" => Val::opt_s(counts.read_frames.map(|n| n.to_string())),
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

/// `bits_per_sample`: a function of the **codec**, not of the container.
///
/// This looks like it should read the container's stored depth and it must not.
/// Measured:
///
/// ```text
///                 bits_per_sample  bits_per_raw_sample  stsd sample_size
/// pcm_s16le wav        16                N/A                  16
/// pcm_s24le mov        24                 24                  16
/// aac       mp4         0                N/A                  16
/// ```
///
/// AAC's sample entry says 16 and the reference prints **0**; `pcm_s24le`'s
/// sample entry also says 16 and the reference prints **24**. Neither follows
/// the container. Both follow the codec: zero for anything compressed, and the
/// PCM flavour's own width for PCM.
///
/// # The PCM table is measured, not derived
///
/// The width is the flavour's stored bits, which the name states — except for
/// A-law and mu-law, which are **8** despite decoding to `s16`. Companding is
/// exactly the case a rule derived from the sample format gets wrong.
///
/// ```text
/// pcm_s16le  -> 16     pcm_u8     ->  8     pcm_alaw  -> 8
/// pcm_s24le  -> 24     pcm_s8     ->  8     pcm_mulaw -> 8
/// pcm_s32le  -> 32     pcm_f32le  -> 32     pcm_f64le -> 64
/// ```
const fn bits_per_sample(codec: Option<CodecId>) -> u32 {
    let Some(codec) = codec else { return 0 };
    match codec {
        CodecId::PcmU8 | CodecId::PcmS8 | CodecId::PcmAlaw | CodecId::PcmMulaw => 8,
        CodecId::PcmS16le | CodecId::PcmS16be => 16,
        CodecId::PcmS24le | CodecId::PcmS24be => 24,
        CodecId::PcmS32le | CodecId::PcmS32be | CodecId::PcmF32le | CodecId::PcmF32be => 32,
        CodecId::PcmF64le | CodecId::PcmF64be => 64,
        // Zero for every compressed codec, which is a value and not an absence.
        _ => 0,
    }
}

/// `bits_per_raw_sample` for a PCM flavour, where the reference states one.
///
/// **Not derivable, and the obvious rule is wrong.** "Report it when it differs
/// from the sample format's natural depth" explains `pcm_s24le` (24 bits stored
/// in `s32`) and then fails on `pcm_s32le`, which is 32 in `s32` — no
/// difference at all — and is still reported:
///
/// ```text
///            sample_fmt  bits_per_raw_sample
/// pcm_s16le      s16            N/A
/// pcm_s24le      s32             24
/// pcm_s32le      s32             32
/// pcm_f32le      flt            N/A
/// ```
///
/// So it is which decoders happen to set the field, which is an implementation
/// fact rather than a property of the format. D17 says reproduce measured
/// behaviour; this is a measured table and is documented as one rather than
/// dressed up as a rule. Both endiannesses of each were checked.
const fn pcm_raw_sample_bits(codec: CodecId) -> Option<u8> {
    match codec {
        CodecId::PcmS24le | CodecId::PcmS24be => Some(24),
        CodecId::PcmS32le | CodecId::PcmS32be => Some(32),
        _ => None,
    }
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
/// # There used to be a float heuristic here
///
/// The demuxers filed the container's sample depth as `bits_per_raw_sample`, so
/// an AAC track reported 16 and an Opus track 32 where the reference reports
/// `N/A`. This function papered over it by suppressing the value whenever the
/// decoded sample format was floating point — true of every affected stream,
/// and true for the wrong reason.
///
/// `AudioParameters::bits_per_coded_sample` now exists and the demuxers fill
/// that instead, so the heuristic is gone and this is a plain read. Worth
/// noting because the heuristic *worked*: every case it was measured against
/// came out right, which is exactly what makes that kind of fix hard to
/// dislodge later.
fn bits_per_raw_sample(p: &CodecParameters) -> Option<u8> {
    if let Some(v) = p.video.as_ref() {
        return v.bits_per_raw_sample;
    }
    let audio = p.audio.as_ref()?;
    audio
        .bits_per_raw_sample
        .or_else(|| p.codec_id.and_then(pcm_raw_sample_bits))
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
        CodecId::Vorbis => Some("vorbis".to_owned()),
        CodecId::Flac => Some("flac".to_owned()),
        CodecId::Vp8 => Some("vp8".to_owned()),
        // `vp09.<profile>.<level>.<depth>` — probed as `vp09.00.10.08`. Left
        // as the bare four-character form because this build has no VP9 parser,
        // so `profile` and `level` are both unknown and a fabricated
        // `vp09.00.10.08` would be right by coincidence on one file and wrong
        // on the next. Closes when a `vaco-parse-vp9` exists.
        CodecId::Vp9 => Some("vp09".to_owned()),
        CodecId::Mp3 => Some("mp4a.40.34".to_owned()),
        // `mp4a.40.<audioObjectType>` again -- MPEG audio Layer II's ISO
        // 14496-3 object type is 33, one below Layer III's 34 already
        // handled above. Measured directly (`ffmpeg -c:a mp2`, real
        // `ffprobe`) identically across a raw `.mp2` file, MPEG-TS, and
        // MPEG-PS/VOB, confirming this is a fixed per-codec string, not
        // something a container changes (matching this function's own
        // doc comment on `H264`). Layer I (`mp1`) would be object type 32
        // by the same table, but this build's ffmpeg has no `mp1` *encoder*
        // to measure against (decode-only), so it is left unhandled here
        // rather than guessed from the pattern.
        CodecId::Mp2 => Some("mp4a.40.33".to_owned()),
        // `mp4v.<objectTypeIndication, hex>` -- RFC 6381's own grammar for
        // MPEG-4 Part 2 also has a trailing `.<profile_level_indication>`,
        // but every native `mpeg4` encode measured (three resolutions,
        // 176x144 through 1280x720) reports `profile=Simple Profile,
        // level=1` and exactly `mp4v.20`, never a three-part string --
        // this crate cannot tell from what is measurable here whether
        // ffmpeg's own trailing part is simply omitted for level 1, or
        // omitted unconditionally. Recorded as measured for the one case
        // reachable, not extended into the two- or three-digit forms other
        // encoders' Core/Main/Advanced-Simple profiles might need.
        CodecId::Mpeg4 => Some("mp4v.20".to_owned()),
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
        let set = vaco_format_core::Disposition::by_name(name).is_some_and(|f| d.contains(f));
        e.int(name, i64::from(set))?;
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
        e.tag(k, v)?;
    }
    e.tf().close()
}

/// What `-show_data`, `-data_dump_format` and `-show_data_hash` asked for.
///
/// A struct rather than three parameters because the *order* of the two extra
/// fields is part of the contract — `data` before `data_hash`, measured — and
/// keeping them together is what makes that visible at the one call site.
#[derive(Clone, Copy, Default, Debug)]
pub struct PayloadOpts {
    /// `-show_data`.
    pub data: Option<DumpFormat>,
    /// `-show_data_hash <alg>`.
    pub hash: Option<HashAlg>,
}

/// One `packet` section.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn packet<W: Write>(
    e: &mut Emit<'_, W>,
    pkt: &Packet,
    stream: Option<&Stream>,
    payload: PayloadOpts,
) -> Result<()> {
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
    //
    // A duration of **zero prints `N/A`**, which is not how `pts` behaves — a
    // pts of 0 prints 0. The reference's duration printer treats 0 as "no
    // value" where its timestamp printer only treats `AV_NOPTS_VALUE` that
    // way. Observed on both, in the same section, three fields apart.
    let ticks = pkt
        .duration_ts()
        .or_else(|| pkt.duration.to_ticks(tb))
        .filter(|t| *t != 0);
    e.field(t, "duration", &Val::opt_i(ticks))?;
    // Derived from the *ticks*, not from the microsecond `Duration`, so that
    // `duration_time` is `duration × time_base` exactly as the reference
    // computes it. Going through microseconds rounds twice: 1024 ticks at
    // 1/44100 is 23219.95 µs, and the second rounding has nothing left to
    // recover the sixth decimal from.
    e.field(t, "duration_time", &Val::opt_f(secs(ticks)))?;
    e.field(t, "size", &Val::F(pkt.payload().len() as f64))?;
    e.field(t, "pos", &Val::opt_s(pkt.pos.map(|p| p.to_string())))?;
    e.field(
        t,
        "flags",
        &Val::s(num::packet_flags(
            pkt.flags.contains(PacketFlags::KEY),
            pkt.flags.contains(PacketFlags::DISCARD),
            pkt.flags.contains(PacketFlags::CORRUPT),
        )),
    )?;
    if let Some(format) = payload.data {
        e.field(t, "data", &Val::s(format.render(pkt.payload())))?;
    }
    if let Some(alg) = payload.hash {
        e.field(
            t,
            "data_hash",
            &Val::opt_s(alg.labelled_digest(pkt.payload())),
        )?;
    }
    packet_side_data(e, pkt)?;
    e.tf().close()
}

/// The packet's `side_data_list`, when it carries any.
///
/// `Skip Samples` and `MPEGTS Stream ID` are measured, because they are the
/// only kinds our demuxers produce: MP4 and Matroska both attach `Skip
/// Samples` to the first audio packet after a discontinuity, and MPEG-TS
/// attaches `MPEGTS Stream ID` to every packet. The reference prints
///
/// ```text
/// [SIDE_DATA]
/// side_data_type=Skip Samples
/// skip_samples=1024
/// discard_padding=0
/// skip_reason=0
/// discard_reason=0
/// [/SIDE_DATA]
/// [SIDE_DATA]
/// side_data_type=MPEGTS Stream ID
/// id=224
/// [/SIDE_DATA]
/// ```
///
/// `skip_reason`/`discard_reason` are 0 in every file measured so far — no
/// producer in this workspace has a source for anything else — but they are
/// real fields on [`PacketSideData::SkipSamples`] now, not a literal `0`
/// written here regardless of what the packet says, so a producer that ever
/// learns a reason still prints correctly.
///
/// The exact `DurationTicks` entry is internal timing metadata and is never a
/// user-visible side-data block. The other three kinds print their type name
/// and nothing else. Their names are **not** measured — no demuxer in this
/// build emits one — so they are marked as such rather than presented as
/// observed.
fn packet_side_data<W: Write>(e: &mut Emit<'_, W>, pkt: &Packet) -> Result<()> {
    if !pkt
        .side_data
        .iter()
        .any(|datum| !matches!(datum, PacketSideData::DurationTicks(_)))
    {
        return Ok(());
    }
    e.tf().open(SectionId::PACKET_SIDE_DATA_LIST)?;
    for datum in &pkt.side_data {
        if matches!(datum, PacketSideData::DurationTicks(_)) {
            continue;
        }
        let name = packet_side_data_name(datum);
        e.tf().open_typed(SectionId::PACKET_SIDE_DATA, name)?;
        e.str("side_data_type", name)?;
        match *datum {
            PacketSideData::SkipSamples {
                start,
                end,
                skip_reason,
                discard_reason,
            } => {
                e.int("skip_samples", i64::from(start))?;
                e.int("discard_padding", i64::from(end))?;
                e.int("skip_reason", i64::from(skip_reason))?;
                e.int("discard_reason", i64::from(discard_reason))?;
            }
            PacketSideData::MpegtsStreamId(id) => {
                e.int("id", i64::from(id))?;
            }
            _ => {}
        }
        e.tf().close()?;
    }
    e.tf().close()
}

/// Measured for `SkipSamples` and `MpegtsStreamId`; the rest are unverified
/// (see above).
const fn packet_side_data_name(d: &PacketSideData) -> &'static str {
    match d {
        PacketSideData::Palette(_) => "Palette",
        PacketSideData::NewExtradata(_) => "New Extradata",
        PacketSideData::DisplayMatrix(_) => "Display Matrix",
        PacketSideData::SkipSamples { .. } => "Skip Samples",
        PacketSideData::MpegtsStreamId(_) => "MPEGTS Stream ID",
        PacketSideData::DurationTicks(_) => "Duration Ticks",
        _ => "Unknown",
    }
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
    e.str("id", &c.id.to_string())?;
    e.str("time_base", &num::rational(tb))?;
    e.ts("start", c.start.ticks())?;
    e.duration("start_time", secs(c.start.ticks()))?;
    e.ts("end", c.end.ticks())?;
    e.duration("end_time", secs(c.end.ticks()))?;
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
    counts: &dyn Fn(u32) -> Counts,
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
    e.int("program_id", p.id)?;
    e.int_opt("program_num", p.program_num)?;
    e.int("nb_streams", i64::try_from(members).unwrap_or(i64::MAX))?;
    e.int_opt("pmt_pid", p.pmt_pid.map(i64::from))?;
    e.int_opt("pcr_pid", p.pcr_pid.map(i64::from))?;
    tags(e, SectionId::PROGRAM_TAGS, &p.metadata)?;
    e.tf().open(SectionId::PROGRAM_STREAMS)?;
    for index in &p.stream_indices {
        if let Some(s) = streams.iter().find(|s| s.index == *index) {
            e.tf().open(SectionId::PROGRAM_STREAM)?;
            stream_fields(e, s, show_ids, counts(s.index))?;
            disposition(e, SectionId::PROGRAM_STREAM_DISPOSITION, s.disposition)?;
            tags(e, SectionId::PROGRAM_STREAM_TAGS, &s.metadata)?;
            e.tf().close()?;
        }
    }
    e.tf().close()?;
    e.tf().close()
}

/// One `[STREAM_GROUP]` section.
///
/// Field order and names **measured** with `ffprobe 9.0.1 -show_stream_groups
/// -of flat` on a HEIF `grid` file: `index`, `id`, `nb_streams`, `type`, then
/// one `[COMPONENT]` (`nb_tiles`, `coded_width`, `coded_height`,
/// `horizontal_offset`, `vertical_offset`, `width`, `height`) holding one
/// `[SUBCOMPONENT]` per tile (`stream_index`, `tile_horizontal_offset`,
/// `tile_vertical_offset`), then the disposition, the tags and the member
/// streams in full.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn stream_group<W: Write>(
    e: &mut Emit<'_, W>,
    g: &StreamGroup,
    streams: &[Stream],
    show_ids: bool,
    counts: &dyn Fn(u32) -> Counts,
) -> Result<()> {
    let members = g
        .stream_indices
        .iter()
        .filter(|i| streams.iter().any(|s| s.index == **i))
        .count();
    e.tf().open(SectionId::STREAM_GROUP)?;
    e.int("index", i64::from(g.index.0))?;
    if show_ids {
        e.str("id", &num::id(g.id))?;
    }
    e.int("nb_streams", i64::try_from(members).unwrap_or(i64::MAX))?;
    match &g.kind {
        StreamGroupKind::TileGrid(grid) => {
            e.str("type", "Tile Grid")?;
            e.tf().open(SectionId::STREAM_GROUP_COMPONENTS)?;
            e.tf().open(SectionId::STREAM_GROUP_COMPONENT)?;
            e.int(
                "nb_tiles",
                i64::try_from(g.stream_indices.len()).unwrap_or(i64::MAX),
            )?;
            e.int("coded_width", i64::from(grid.coded_width))?;
            e.int("coded_height", i64::from(grid.coded_height))?;
            e.int("horizontal_offset", i64::from(grid.horizontal_offset))?;
            e.int("vertical_offset", i64::from(grid.vertical_offset))?;
            e.int("width", i64::from(grid.output_width))?;
            e.int("height", i64::from(grid.output_height))?;
            e.tf().open(SectionId::SUBCOMPONENTS)?;
            for (index, (h, v)) in g.stream_indices.iter().zip(&grid.tile_offsets) {
                e.tf().open(SectionId::SUBCOMPONENT)?;
                e.int("stream_index", i64::from(*index))?;
                e.int("tile_horizontal_offset", i64::from(*h))?;
                e.int("tile_vertical_offset", i64::from(*v))?;
                e.tf().close()?;
            }
            e.tf().close()?;
            e.tf().close()?;
            e.tf().close()?;
        }
        // `StreamGroupKind` is `#[non_exhaustive]`; a kind this printer does
        // not know is named rather than dropped.
        _ => e.str("type", "Unknown")?,
    }
    disposition(e, SectionId::STREAM_GROUP_DISPOSITION, g.disposition)?;
    tags(e, SectionId::STREAM_GROUP_TAGS, &g.metadata)?;
    e.tf().open(SectionId::STREAM_GROUP_STREAMS)?;
    for index in &g.stream_indices {
        if let Some(s) = streams.iter().find(|s| s.index == *index) {
            e.tf().open(SectionId::STREAM_GROUP_STREAM)?;
            stream_fields(e, s, show_ids, counts(s.index))?;
            disposition(e, SectionId::STREAM_GROUP_STREAM_DISPOSITION, s.disposition)?;
            tags(e, SectionId::STREAM_GROUP_STREAM_TAGS, &s.metadata)?;
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

    #[test]
    fn mime_codec_string_is_bare_vorbis() {
        // ffprobe -show_entries stream=mime_codec_string v.ogg  ->  vorbis
        let vorbis = CodecParameters::audio().with_codec(CodecId::Vorbis);
        assert_eq!(mime_codec_string(&vorbis).as_deref(), Some("vorbis"));
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
            stream_value(
                &field(name),
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false,
            )
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

    /// Fixing HEVC's `nal_length_size` population from `hvcC` (so the
    /// raw/MPEG-TS muxers can Annex-B-convert a copied HEVC stream) had
    /// the side effect of also making `vaco-probe` print
    /// `is_avc`/`nal_length_size` for HEVC, which the reference never does —
    /// measured directly (`ffprobe -bitexact -show_streams` on an `hvc1`/MP4
    /// HEVC stream has neither field; `ffmpeg -h decoder=hevc` has no such
    /// private options at all). The two fields must stay H.264-only even
    /// when `VideoParameters.nal_length_size` is populated for another
    /// codec.
    #[test]
    fn is_avc_and_nal_length_size_are_h264_only_even_when_populated_for_hevc() {
        fn field(name: &str) -> Field {
            crate::fields::STREAM
                .iter()
                .find(|f| f.name == name)
                .copied()
                .expect("in the table")
        }
        let stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let mut p = CodecParameters::video().with_codec(CodecId::Hevc);
        if let Some(v) = p.video.as_mut() {
            v.nal_length_size = Some(4);
        }
        let render = |name: &str| {
            stream_value(
                &field(name),
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false,
            )
        };
        assert!(matches!(render("is_avc"), Val::Absent));
        assert!(matches!(render("nal_length_size"), Val::Absent));
    }

    /// `quarter_sample`/`divx_packed` are MPEG-4 Part 2's own pair, gated
    /// on `codec_id` the same way `is_avc`/`nal_length_size` are gated on
    /// H.264 just above -- present and rendered as `"true"`/`"false"` for
    /// `Mpeg4`, absent for every other codec even when the underlying
    /// `VideoParameters` fields happen to be populated.
    #[test]
    fn quarter_sample_and_divx_packed_are_mpeg4_only() {
        fn field(name: &str) -> Field {
            crate::fields::STREAM
                .iter()
                .find(|f| f.name == name)
                .copied()
                .expect("in the table")
        }
        let stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let render = |codec: CodecId, quarter: Option<bool>, packed: Option<bool>, name: &str| {
            let mut p = CodecParameters::video().with_codec(codec);
            if let Some(v) = p.video.as_mut() {
                v.quarter_sample = quarter;
                v.divx_packed = packed;
            }
            stream_value(
                &field(name),
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false,
            )
        };
        let text = |v: Val| match v {
            Val::S(s) => Some(s),
            _ => None,
        };
        assert_eq!(
            text(render(
                CodecId::Mpeg4,
                Some(false),
                Some(false),
                "quarter_sample"
            ))
            .as_deref(),
            Some("false")
        );
        assert_eq!(
            text(render(
                CodecId::Mpeg4,
                Some(false),
                Some(false),
                "divx_packed"
            ))
            .as_deref(),
            Some("false")
        );
        // Another codec, fields populated anyway: still absent.
        assert!(matches!(
            render(CodecId::H264, Some(false), Some(false), "quarter_sample"),
            Val::Absent
        ));
        assert!(matches!(
            render(CodecId::H264, Some(false), Some(false), "divx_packed"),
            Val::Absent
        ));
        assert_eq!(field("quarter_sample").absent, crate::fields::Absent::Omit);
        assert_eq!(field("divx_packed").absent, crate::fields::Absent::Omit);
    }

    /// `ts_id`/`ts_packetsize` read back
    /// through `Stream::metadata` — the one channel `vaco-demux-mpegts` has
    /// to hand them to this crate — as *strings*, not integers. Absent (not
    /// `"0"`) when the key is missing, same `Omit` policy as
    /// `nal_length_size` above, so a non-TS container prints neither field
    /// at all. `Str`, not `Int`: `ffprobe -of flat`/`-of json` quote both
    /// values (`ts_id="1"`) despite the digits, found by a differential run
    /// against the reference on a plain, unmutated MPEG-TS file.
    #[test]
    fn ts_id_and_ts_packetsize_come_from_stream_metadata() {
        let field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "ts_id")
            .copied()
            .expect("in the table");
        assert_eq!(field.absent, crate::fields::Absent::Omit);
        assert!(!field.ty.is_int());

        let mut stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let p = CodecParameters::video().with_codec(CodecId::H264);
        let string = |v: Val| match v {
            Val::S(s) => Some(s),
            _ => None,
        };

        assert!(matches!(
            stream_value(
                &field,
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false
            ),
            Val::Absent
        ));

        stream.metadata_set("ts_id", "1");
        stream.metadata_set("ts_packetsize", "188");
        assert_eq!(
            string(stream_value(
                &field,
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false
            )),
            Some("1".to_owned())
        );
        let packetsize_field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "ts_packetsize")
            .copied()
            .expect("in the table");
        assert_eq!(
            string(stream_value(
                &packetsize_field,
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false
            )),
            Some("188".to_owned())
        );
    }

    /// The other half of the fix: `ts_id`/`ts_packetsize` must not *also* appear
    /// as `TAG:` lines — they are dedicated fields, read by the arms tested
    /// above, not user-visible container metadata. A real tag (`language`,
    /// say) must still come through untouched.
    #[test]
    fn ts_id_and_ts_packetsize_are_filtered_out_of_the_visible_tags() {
        let mut stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        stream.metadata_set("ts_id", "1");
        stream.metadata_set("ts_packetsize", "188");
        stream.metadata_set("language", "eng");
        let visible = stream_visible_metadata(&stream);
        assert_eq!(visible, vec![("language".to_owned(), "eng".to_owned())]);
    }

    /// A codec with no profile is *absent*, not the string `unknown`.
    ///
    /// The distinction is invisible in every writer but `json` and `xml`, which
    /// are the two that omit unavailable optional fields — and those are the
    /// two where the reference prints no `profile` key at all. Measured on a
    /// FLAC stream:
    ///
    /// ```sh
    /// ffprobe -v quiet -of json    -show_entries stream=profile f.flac  # {}
    /// ffprobe -v quiet -of default -show_entries stream=profile f.flac  # profile=unknown
    /// ```
    #[test]
    fn a_codec_without_a_profile_is_absent_not_the_word() {
        let field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "profile")
            .copied()
            .expect("in the table");
        let stream = Stream::new(0, MediaType::Audio, vaco_core::Rational::new(1, 1000));
        let p = CodecParameters::audio().with_codec(CodecId::Flac);
        let v = stream_value(
            &field,
            &stream,
            &p,
            Some(MediaType::Audio),
            Counts::NONE,
            false,
        );
        assert!(matches!(v, Val::Absent), "{v:?}");
        // …and the table still carries the word, so `-of default` prints it.
        assert_eq!(field.absent, crate::fields::Absent::Word("unknown"));
    }

    /// `-bitexact` prints the raw numeric profile instead of the library
    /// name; `level` is untouched because it was never a name. Measured
    /// against `ffprobe 8.1` on H.264 (`High`/`100`) and AV1 (`Main`/`0`);
    /// see `stream_value`'s `"profile"` arm for the other two codecs probed.
    #[test]
    fn bitexact_swaps_the_profile_name_for_its_number() {
        let field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "profile")
            .copied()
            .expect("in the table");
        let stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        p.profile = Some(vaco_codec_core::Profile::new(100, "High"));
        let text = |v: Val| match v {
            Val::S(s) => Some(s),
            _ => None,
        };
        let named = stream_value(
            &field,
            &stream,
            &p,
            Some(MediaType::Video),
            Counts::NONE,
            false,
        );
        assert_eq!(text(named).as_deref(), Some("High"));
        let numeric = stream_value(
            &field,
            &stream,
            &p,
            Some(MediaType::Video),
            Counts::NONE,
            true,
        );
        assert_eq!(text(numeric).as_deref(), Some("100"));
    }

    /// A profile with no name at all (VP8's bare `version` number, or an
    /// H.264 `profile_idc` the standard never assigned a name) prints the
    /// number in *both* modes — measured, `ffprobe` never prints an empty
    /// `profile=` value.
    #[test]
    fn an_unnamed_profile_is_numeric_even_without_bitexact() {
        let field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "profile")
            .copied()
            .expect("in the table");
        let stream = Stream::new(0, MediaType::Video, vaco_core::Rational::new(1, 1000));
        let mut p = CodecParameters::video().with_codec(CodecId::Vp8);
        p.profile = Some(vaco_codec_core::Profile::new(0, ""));
        let v = stream_value(
            &field,
            &stream,
            &p,
            Some(MediaType::Video),
            Counts::NONE,
            false,
        );
        let text = match v {
            Val::S(s) => Some(s),
            _ => None,
        };
        assert_eq!(text.as_deref(), Some("0"));
    }

    /// `bits_per_raw_sample` is a codec property. The container's own sample
    /// depth is a different field — `bits_per_coded_sample` — and the demuxers
    /// now fill that one, so this is a plain read with no heuristic in it.
    #[test]
    fn bits_per_raw_sample_is_a_codec_fact_not_a_container_one() {
        let mut video = CodecParameters::video().with_codec(CodecId::H264);
        if let Some(v) = video.video.as_mut() {
            v.bits_per_raw_sample = Some(8);
        }
        assert_eq!(bits_per_raw_sample(&video), Some(8));

        // AAC: the container states 16, the reference prints `N/A`. The 16 now
        // lands in `bits_per_coded_sample` and never reaches this field, which
        // is what replaced the float-format heuristic that used to sit here.
        let mut aac = CodecParameters::audio().with_codec(CodecId::Aac);
        if let Some(a) = aac.audio.as_mut() {
            a.bits_per_coded_sample = Some(16);
            a.format = Some(vaco_sampfmt::SampleFmt::F32P);
        }
        assert_eq!(bits_per_raw_sample(&aac), None);

        // And a codec that genuinely states one still reports it.
        let mut pcm = CodecParameters::audio().with_codec(CodecId::Pcm);
        if let Some(a) = pcm.audio.as_mut() {
            a.bits_per_raw_sample = Some(24);
        }
        assert_eq!(bits_per_raw_sample(&pcm), Some(24));
    }

    /// `bits_per_sample` follows the codec, never the container — the trap this
    /// pair sets. AAC's sample entry says 16 and the reference prints 0.
    #[test]
    fn bits_per_sample_is_zero_for_every_codec_modelled_today() {
        assert_eq!(bits_per_sample(Some(CodecId::Aac)), 0);
        assert_eq!(bits_per_sample(Some(CodecId::Opus)), 0);
        assert_eq!(bits_per_sample(None), 0);
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

        // Measured (`ffmpeg -c:a mp2`, real `ffprobe`, identically across a
        // raw `.mp2` file, MPEG-TS, and MPEG-PS/VOB): `mp4a.40.33`, needing
        // neither `profile` nor `level` to be set.
        let mp2 = CodecParameters::audio().with_codec(CodecId::Mp2);
        assert_eq!(mime_codec_string(&mp2).as_deref(), Some("mp4a.40.33"));

        // Measured (`ffmpeg -c:v mpeg4`, real `ffprobe`, three resolutions):
        // `mp4v.20`, also needing neither field set on the measurable case
        // (native `mpeg4` always reports Simple Profile / level 1).
        let mpeg4 = CodecParameters::video().with_codec(CodecId::Mpeg4);
        assert_eq!(mime_codec_string(&mpeg4).as_deref(), Some("mp4v.20"));
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

    /// Measured (`ffprobe 8.1`): a stream with no known field order omits
    /// `field_order` under `json`'s default `-show_optional_fields auto`
    /// (`WriterFlags::SUPPRESS_OPTIONAL`) rather than printing the literal
    /// string `"unknown"` unconditionally. `stream_value` must answer
    /// `Val::Absent` for that case, the same shape `colour()` already gives
    /// `color_range`/`color_space`/`color_transfer`/`color_primaries` — not a
    /// concrete `Val::S("unknown")`, which would bypass the optional-field
    /// policy entirely (the bug this test pins, found diffing a real PNG
    /// probe against the reference: ours printed the field, the reference
    /// did not).
    #[test]
    fn field_order_is_absent_not_the_literal_string_unknown() {
        let field = crate::fields::STREAM
            .iter()
            .find(|f| f.name == "field_order")
            .copied()
            .expect("in the table");
        assert_eq!(field.absent, crate::fields::Absent::Word("unknown"));

        let stream = Stream::new(0, MediaType::Video, Rational::new(1, 1000));
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        if let Some(v) = p.video.as_mut() {
            v.field_order = vaco_codec_core::FieldOrder::Unknown;
        }
        assert!(matches!(
            stream_value(
                &field,
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false
            ),
            Val::Absent
        ));

        if let Some(v) = p.video.as_mut() {
            v.field_order = vaco_codec_core::FieldOrder::TopFirst;
        }
        let string = |v: Val| match v {
            Val::S(s) => Some(s),
            _ => None,
        };
        assert_eq!(
            string(stream_value(
                &field,
                &stream,
                &p,
                Some(MediaType::Video),
                Counts::NONE,
                false
            )),
            Some("tt".to_owned())
        );
    }
}
