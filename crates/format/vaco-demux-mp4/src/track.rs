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
fn hvcc_length_size(data: &[u8]) -> Option<u8> {
    data.get(21).map(|b| (b & 0x03) + 1)
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
}
