//! Turning one `trak` into one [`Stream`].
//!
//! Every number here is one `ffprobe` prints, and every rule was measured
//! against `ffprobe 8.1` rather than inferred. The measurements and the exact
//! commands are in `docs/format/vaco-demux-mp4.md`; the summary is:
//!
//! * `duration_ts` is `min(edit-list duration, media limit)`, where the media
//!   limit is `min(mdhd.duration, sum of the sample durations)` — **not**
//!   `mdhd.duration`, which is what `vaco-format-isom`'s
//!   `Track::reported_duration` returns.
//! * `bit_rate` divides by the media limit, not by `duration_ts`.
//! * `sample_aspect_ratio` comes from `pasp`, or from `tkhd` dimensions that
//!   disagree with the sample entry's, and is otherwise left for the bitstream
//!   parser to supply.

use vaco_codec_core::{AudioParameters, CodecParameters, VideoParameters};
use vaco_color::{ColorPrimaries, MatrixCoefficients, TransferCharacteristic};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::{Disposition, Stream};
use vaco_format_isom::stsd::{ConfigFlavour, SampleEntry};
use vaco_format_isom::{Language, Track, esds, fixed, stsd};

/// Media-timescale quantities derived from the sample table or the fragments.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MediaTotals {
    /// Sum of every sample's duration.
    pub duration: i64,
    /// Sum of every sample's size.
    pub bytes: u64,
    /// Samples, from `stsz` — zero for a fragmented track, which has none.
    pub count: u32,
    /// The most common `stts` delta, or zero when there is no `stts`.
    pub common_delta: u32,
    /// Samples the deltas actually account for.
    pub timed: u64,
}

/// `min(mdhd.duration, total sample duration)`.
///
/// **Measured.** `ffprobe` divides by this to get `bit_rate`, and clamps
/// `duration_ts` to it. A zero `mdhd.duration` — every fragmented file — means
/// "no statement", not "zero length".
pub(crate) fn media_limit(mdhd_duration: u64, totals: &MediaTotals) -> i64 {
    let table = totals.duration.max(0);
    let stated = i64::try_from(mdhd_duration).unwrap_or(i64::MAX);
    if mdhd_duration == 0 {
        table
    } else if table == 0 {
        stated
    } else {
        stated.min(table)
    }
}

/// `bit_rate`, in bits per second.
///
/// `bytes * 8 * timescale / limit`. The rounding differs between the two paths
/// and both are measured: the sample-table path truncates (a `stts` total of
/// 25 650 gives 283 873 where the exact value is 283 873.56), and the fragment
/// path rounds to nearest (67 753.86 prints as 67 754).
#[allow(
    clippy::integer_division,
    reason = "a bit rate is an integer count of bits per second; the rounding mode is the measured behaviour and is chosen explicitly by `round_to_nearest`"
)]
pub(crate) fn bit_rate(
    bytes: u64,
    timescale: u32,
    limit: i64,
    round_to_nearest: bool,
) -> Option<u64> {
    if limit <= 0 || timescale == 0 || bytes == 0 {
        return None;
    }
    let num = i128::from(bytes)
        .checked_mul(8)?
        .checked_mul(i128::from(timescale))?;
    let den = i128::from(limit);
    let v = if round_to_nearest {
        num.checked_add(den / 2)? / den
    } else {
        num / den
    };
    u64::try_from(v).ok()
}

/// Build the container-level codec description from one sample entry.
pub(crate) fn codec_parameters(
    entry: &SampleEntry<'_>,
    media_type: Option<MediaType>,
    track: &Track<'_>,
) -> CodecParameters {
    let mut params = CodecParameters {
        media_type,
        codec_id: entry.codec(),
        codec_tag: Some(entry.format.0),
        ..CodecParameters::default()
    };
    params.media_type = params
        .media_type
        .or_else(|| params.codec_id.map(vaco_codec_core::CodecId::media_type));

    let mut hevc_length_size = None;
    let mut opus_pre_skip = None;
    if let Some(config) = entry.config() {
        match config.flavour {
            ConfigFlavour::Esds => {
                // The extradata is the `DecoderSpecificInfo` inside the
                // descriptor tree, never the box body: an `esds` carries
                // buffer sizes and bit rates around it.
                if let Ok(full) = vaco_format_isom::boxes::FullBox::parse(config.data, 0)
                    && let Ok(es) = esds::EsDescriptor::parse(&full)
                {
                    params.extradata = es.decoder_specific.map(<[u8]>::to_vec);
                }
            }
            ConfigFlavour::Hvcc => {
                hevc_length_size = hvcc_length_size(config.data);
                params.extradata = Some(config.data.to_vec());
            }
            ConfigFlavour::Alac => {
                // `alac` is a full box: `config.data` starts with 4 bytes
                // of version+flags (per `CodecConfig::data`'s own doc,
                // "for a full box the version and flags are still
                // present"), and those 4 bytes are not part of the
                // `ALACSpecificConfig` a decoder actually reads --
                // `vaco-codec-alac`'s `AlacCookie::parse` expects either
                // the bare 24(+)-byte record or its `frma`-wrapped
                // "Compatibility" shape, neither of which carries this
                // box's own header. Handing over the un-stripped 28 bytes
                // shifts every field: `frame_length` (the first 4 bytes
                // of the real record) reads as the version+flags' `0`
                // instead, and every packet whose `partialFrame` bit
                // relies on that cookie value to know its own sample
                // count decodes as zero samples -- measured end to end
                // on a real `ffmpeg`-produced `.m4a`: `vaco` decoded
                // exactly the file's one genuinely `partialFrame`-tagged
                // (explicit count) packet, its short final frame, and
                // silently dropped the other 21 to zero samples each,
                // exiting 0 having produced about 2.5% of the audio.
                params.extradata = config.data.get(4..).map(<[u8]>::to_vec);
            }
            ConfigFlavour::Dfla => {
                // `dfLa` is a full box (the FLAC-in-ISOBMFF mapping's
                // `FLACSpecificBox extends FullBox('dfLa', 0, 0)`): its
                // payload is 4 bytes of version+flags, then one or more
                // FLAC metadata blocks verbatim, `STREAMINFO` first. This
                // project's own canonical `extradata` shape for FLAC is
                // `"fLaC" +` those same metadata blocks (`FlacEncoder::
                // extradata`'s convention; Matroska's `A_FLAC`
                // `CodecPrivate` matches it) -- so this replaces the box's
                // own version+flags with that magic rather than keeping
                // them, the same "container-specific header out,
                // project-wide magic in" shape `ConfigFlavour::Dops` uses
                // above. Measured end to end: without this,
                // `vaco-parse-audio-misc::flac::FlacParser` -- reached
                // generically through `ParserProvider` -- read straight
                // into the version+flags and the metadata block's own
                // header as if they were `STREAMINFO` data, reporting
                // `channels=1`, `bits_per_raw_sample=1` for a real 48 kHz
                // stereo 16-bit file, and `vaco -c:a copy` of the same file
                // back to `.mp4` failed outright ("FLAC extradata is not a
                // recognised STREAMINFO shape") because the corrupted
                // extradata did not even round-trip through this crate's
                // own mux side.
                if let Some(blocks) = config.data.get(4..) {
                    let mut extradata = b"fLaC".to_vec();
                    extradata.extend_from_slice(blocks);
                    params.extradata = Some(extradata);
                }
            }
            ConfigFlavour::Dops => {
                // `dOps` is not "`OpusHead` minus its magic and version
                // byte" (a claim `vaco-format-isom::writer::dops`'s doc
                // comment used to make, and this crate's write side used to
                // trust): it keeps the version byte and drops only the
                // 8-byte magic, and every multi-byte field is big-endian,
                // not little. Measured against a real `ffmpeg -c:a libopus
                // -f mp4` fixture's own box: `00 02 01 38 00 00 bb 80 00 00
                // 00` — version 0, channels 2, pre_skip 0x0138 = 312
                // (`ffprobe` reports `initial_padding=312` for the same
                // file), rate 0x0000bb80 = 48000, gain 0, family 0. Every
                // other container this project reads (Ogg, Matroska) hands
                // `vaco-codec-opus` a real magic-prefixed, little-endian
                // `OpusHead`, so this reconstructs one rather than passing
                // `dOps`'s own bytes through unshaped — the same conversion
                // `vaco-parse-opus::head::IdentificationHeader::to_opus_head`
                // documents the reference itself performing, re-implemented
                // here rather than taking a `vaco-parse-*` dependency (D14.1).
                if let Some((head, pre_skip)) = dops_to_opus_head(config.data) {
                    params.extradata = Some(head);
                    opus_pre_skip = Some(pre_skip);
                }
            }
            _ => params.extradata = Some(config.data.to_vec()),
        }
    }

    if let Some(v) = entry.visual {
        let mut video = VideoParameters {
            width: u32::from(v.width),
            height: u32::from(v.height),
            coded_width: u32::from(v.width),
            coded_height: u32::from(v.height),
            nal_length_size: hevc_length_size,
            ..VideoParameters::default()
        };
        video.sample_aspect_ratio = sample_aspect_ratio(entry, track, &v).unwrap_or(Rational::ZERO);
        if let Some(colour) = entry.colour() {
            video.color.primaries = colour
                .primaries
                .and_then(|c| ColorPrimaries::from_u8(c.try_into().unwrap_or(u8::MAX)))
                .unwrap_or_default();
            video.color.transfer = colour
                .transfer
                .and_then(|c| TransferCharacteristic::from_u8(c.try_into().unwrap_or(u8::MAX)))
                .unwrap_or_default();
            video.color.matrix = colour
                .matrix
                .and_then(|c| MatrixCoefficients::from_u8(c.try_into().unwrap_or(u8::MAX)))
                .unwrap_or_default();
            if colour.full_range {
                video.color.range = vaco_color::ColorRange::Full;
            }
        }
        params.video = Some(video);
    } else if let Some(a) = entry.audio {
        params.audio = Some(AudioParameters {
            sample_rate: a.rate_hz(),
            // The container's stored depth, which is `bits_per_coded_sample`
            // and not `bits_per_raw_sample`. Filing it as the latter made an
            // AAC track report 16 where the reference reports N/A.
            bits_per_coded_sample: (a.sample_size > 0 && a.sample_size <= 64)
                .then_some(a.sample_size as u8),
            initial_padding: opus_pre_skip.map_or(0, u32::from),
            ..AudioParameters::default()
        });
    }
    params
}

/// The length-prefix width an `hvcC` declares, in bytes.
///
/// `HEVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §8.3.3.1) is not a full
/// box, so `data[0]` is `configurationVersion` directly. `lengthSizeMinusOne`
/// is the low two bits of the 22nd byte (index 21), the same relative
/// position `avcC`'s field occupies in its own, shorter fixed header — one
/// past the last of six reserved-bit groups (`min_spatial_segmentation_idc`,
/// `parallelismType`, `chromaFormat`, `bitDepthLumaMinus8`,
/// `bitDepthChromaMinus8`, `avgFrameRate`) that all precede it.
///
/// The field is 2 bits wide but only three of its four values are defined:
/// ISO/IEC 14496-15 §8.3.3.1 states `lengthSizeMinusOne` may be 0, 1 or 3
/// (a 1-, 2- or 4-byte length field). 2 — a 3-byte length field — is
/// reserved and no conformant writer emits it; treat it as "unknown" rather
/// than reporting a length size the format does not define. Before this, a
/// crafted `hvcC` with the reserved bit pattern reached `nal_length_size=3`
/// straight through to probe output, unvalidated — the same class of bug
/// `fuzz/fuzz_targets/registry_discovery.rs` found in `bits_per_raw_sample`
/// (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0, via JPEG's `precision`),
/// just not yet through this specific field.
fn hvcc_length_size(data: &[u8]) -> Option<u8> {
    match data.get(21).map(|b| b & 0x03) {
        Some(raw @ (0 | 1 | 3)) => Some(raw + 1),
        _ => None,
    }
}

/// Build a magic-prefixed, little-endian `OpusHead` blob from an MP4 `dOps`
/// box's payload, plus the `pre_skip` field on its own (so the caller does
/// not have to re-parse the head it just built to find it again).
///
/// Layout: `version(1) channels(1) pre_skip(16 BE) input_sample_rate(32 BE)
/// output_gain(16 BE) channel_mapping_family(1)`, optionally followed by a
/// mapping table (`stream_count(1) coupled_count(1) channel_mapping(N)`) when
/// the family is non-zero — copied through verbatim since every field in it
/// is single-byte and therefore endian-agnostic.
fn dops_to_opus_head(data: &[u8]) -> Option<(Vec<u8>, u16)> {
    let pre_skip = u16::from_be_bytes(data.get(2..4)?.try_into().ok()?);
    let rate = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?);
    let gain = u16::from_be_bytes(data.get(8..10)?.try_into().ok()?);
    let mut out = Vec::new();
    out.extend_from_slice(b"OpusHead");
    out.push(*data.first()?); // version
    out.push(*data.get(1)?); // channel count
    out.extend_from_slice(&pre_skip.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&gain.to_le_bytes());
    out.extend_from_slice(data.get(10..)?);
    Some((out, pre_skip))
}

/// `sample_aspect_ratio`, or `None` to leave it for the bitstream parser.
///
/// **Measured.** `pasp` wins. Failing that, a `tkhd` whose display size
/// disagrees with the sample entry's coded size states the ratio: a `tkhd`
/// width of 320 over a coded width of 160 printed `2:1`. When the two agree,
/// nothing is set here and `sample_aspect_ratio` comes from the elementary
/// stream — which is why an unmodified file still prints `1:1`.
fn sample_aspect_ratio(
    entry: &SampleEntry<'_>,
    track: &Track<'_>,
    visual: &stsd::VisualSampleEntry,
) -> Option<Rational> {
    if let Some(pasp) = entry
        .extension_boxes()
        .find(vaco_format_isom::fourcc::boxes::PASP)
    {
        let mut r = vaco_bitstream::ByteReader::new(pasp.payload);
        let (h, v) = (r.be32(), r.be32());
        if h != 0 && v != 0 {
            return Some(Rational::new(
                i32::try_from(h).unwrap_or(i32::MAX),
                i32::try_from(v).unwrap_or(i32::MAX),
            ));
        }
    }
    let (dw, dh) = (track.header.width, track.header.height);
    let (cw, ch) = (i32::from(visual.width), i32::from(visual.height));
    if cw <= 0 || ch <= 0 || dw.num <= 0 || dh.num <= 0 {
        return None;
    }
    // The display size is 16.16 fixed point; compare it against the coded size
    // as a rational so a fractional width is not silently truncated.
    let same_w = dw
        .checked_sub(Rational::new(cw, 1))
        .is_some_and(Rational::is_zero);
    let same_h = dh
        .checked_sub(Rational::new(ch, 1))
        .is_some_and(Rational::is_zero);
    if same_w && same_h {
        return None;
    }
    let sar = dw
        .checked_div(Rational::new(cw, 1))?
        .checked_div(dh.checked_div(Rational::new(ch, 1))?)?;
    sar.checked_reduced()
}

/// The stream tags a track carries, in the order `ffprobe` prints them.
pub(crate) fn track_metadata(
    track: &Track<'_>,
    vendor: Option<[u8; 4]>,
    compressor: Option<&str>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    // A Macintosh language code is not a packed ISO-639 value and the reference
    // prints no `language` tag for one — measured on a `.mov` whose `mdhd`
    // language field is 0x7FFF.
    let macintosh = matches!(track.media.language, Language::Macintosh(_));
    if track.extended_language.is_some() || !macintosh {
        out.push(("language".to_owned(), track.language_tag().to_owned()));
    }
    if let Some(name) = track.handler_name_str()
        && !name.is_empty()
    {
        out.push(("handler_name".to_owned(), name.to_owned()));
    }
    if let Some(v) = vendor
        && v != [0; 4]
        && v != *b"    "
        && let Ok(s) = core::str::from_utf8(&v)
    {
        out.push(("vendor_id".to_owned(), s.to_owned()));
    }
    if let Some(c) = compressor
        && !c.is_empty()
    {
        out.push(("encoder".to_owned(), c.to_owned()));
    }
    out
}

/// The disposition bits a `tkhd` implies.
pub(crate) fn disposition(track: &Track<'_>) -> Disposition {
    let mut d = Disposition::empty();
    if track.header.is_enabled() {
        d |= Disposition::DEFAULT;
    }
    d
}

/// An empty stream shell with the identity fields filled in.
pub(crate) fn shell(index: u32, track: &Track<'_>, media_type: MediaType) -> Stream {
    let mut s = Stream::new(index, media_type, track.time_base());
    s.id = Some(i64::from(track.header.track_id));
    s.start_time = Timestamp::ZERO;
    s
}

/// The 3×3 `tkhd` matrix as side data, when it is not the identity.
pub(crate) fn display_matrix(track: &Track<'_>) -> Option<[i32; 9]> {
    if track.header.matrix.is_identity() {
        return None;
    }
    let raw = track.header.matrix.raw;
    let mut out = [0i32; 9];
    for (slot, v) in out.iter_mut().zip(raw) {
        *slot = v.cast_signed();
    }
    let _ = fixed::FP16_ONE;
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    fn totals(duration: i64, bytes: u64, count: u32) -> MediaTotals {
        MediaTotals {
            duration,
            bytes,
            count,
            common_delta: 0,
            timed: u64::from(count),
        }
    }

    #[test]
    fn the_media_limit_is_the_smaller_of_mdhd_and_the_table() {
        assert_eq!(media_limit(26_112, &totals(25_600, 0, 0)), 25_600);
        assert_eq!(media_limit(26_112, &totals(30_000, 0, 0)), 26_112);
        // A zero `mdhd.duration` is "no statement", which is every fragmented
        // file, not a zero-length track.
        assert_eq!(media_limit(0, &totals(91_728, 0, 0)), 91_728);
        assert_eq!(media_limit(20_000, &totals(0, 0, 0)), 20_000);
    }

    #[test]
    fn bit_rate_truncates_on_the_sample_table_path() {
        // Measured: a `stts` total of 25 650 over 71 107 bytes at 12 800 ticks
        // prints 283 873, where the exact value is 283 873.56.
        assert_eq!(bit_rate(71_107, 12_800, 25_650, false), Some(283_873));
        assert_eq!(bit_rate(71_107, 12_800, 25_600, false), Some(284_428));
    }

    #[test]
    fn bit_rate_rounds_on_the_fragment_path() {
        // Measured: 17 616 bytes over 91 728 ticks at 44 100 prints 67 754,
        // where the exact value is 67 753.86.
        assert_eq!(bit_rate(17_616, 44_100, 91_728, true), Some(67_754));
        assert_eq!(bit_rate(26_347, 44_100, 134_138, true), Some(69_296));
    }

    #[test]
    fn bit_rate_is_absent_when_there_is_nothing_to_divide() {
        assert_eq!(bit_rate(0, 12_800, 100, false), None);
        assert_eq!(bit_rate(100, 0, 100, false), None);
        assert_eq!(bit_rate(100, 12_800, 0, false), None);
    }

    /// `lengthSizeMinusOne`'s three defined values decode to the byte count
    /// ISO/IEC 14496-15 §8.3.3.1 assigns them; the reserved fourth (2, a
    /// 3-byte length) must not fabricate `nal_length_size=3` — found by
    /// `fuzz/fuzz_targets/registry_discovery.rs`'s sibling audit around
    /// crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0 (a different field,
    /// same class: an unvalidated attacker byte reaching probe output).
    #[test]
    fn hvcc_length_size_rejects_the_reserved_encoding() {
        let mut data = [0u8; 22];
        for (raw, want) in [(0u8, Some(1u8)), (1, Some(2)), (2, None), (3, Some(4))] {
            data[21] = raw;
            assert_eq!(hvcc_length_size(&data), want, "raw lengthSizeMinusOne={raw}");
        }
    }

    #[test]
    fn hvcc_length_size_is_none_for_a_too_short_record() {
        assert_eq!(hvcc_length_size(&[0u8; 21]), None);
    }

    /// A real `dOps` box payload, measured from `ffmpeg -f lavfi -i
    /// "sine=...:sample_rate=48000" -ac 2 -c:a libopus -f mp4`: version 0,
    /// channels 2, `pre_skip` 0x0138 = 312 (`ffprobe` reports
    /// `initial_padding=312` for the same file), rate 0x0000bb80 = 48000,
    /// gain 0, family 0.
    const REAL_DOPS: [u8; 11] = [0x00, 0x02, 0x01, 0x38, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0x00];

    #[test]
    fn dops_to_opus_head_reconstructs_a_real_measured_box() {
        let (head, pre_skip) = dops_to_opus_head(&REAL_DOPS).unwrap();
        assert_eq!(pre_skip, 312);
        assert_eq!(head.get(..8), Some(b"OpusHead".as_slice()));
        assert_eq!(head.get(8).copied(), Some(0), "version");
        assert_eq!(head.get(9).copied(), Some(2), "channel count");
        let pre_skip_le: Option<[u8; 2]> = head.get(10..12).and_then(|b| b.try_into().ok());
        assert_eq!(
            pre_skip_le.map(u16::from_le_bytes),
            Some(312),
            "pre_skip, little-endian now"
        );
        let rate_le: Option<[u8; 4]> = head.get(12..16).and_then(|b| b.try_into().ok());
        assert_eq!(
            rate_le.map(u32::from_le_bytes),
            Some(48_000),
            "input_sample_rate, little-endian now"
        );
        assert_eq!(head.len(), 8 + REAL_DOPS.len());
    }

    #[test]
    fn dops_to_opus_head_is_none_for_a_too_short_record() {
        assert_eq!(dops_to_opus_head(&REAL_DOPS[..8]), None);
    }
}
