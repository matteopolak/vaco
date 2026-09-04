//! `CodecParameters` → one `stsd` sample entry.
//!
//! Every configuration record (`avcC`/`hvcC`/`av1C`/`vpcC`/`dOps`/`dfLa`, and
//! the `DecoderSpecificInfo` inside an `esds`) is written **verbatim** from
//! `CodecParameters::extradata`. This crate never inspects a NAL unit or an
//! `AudioSpecificConfig` to build one — D14.1 forbids a `vaco-format-*` crate
//! from depending on a `vaco-parse-*` one, and a caller whose stream has no
//! extradata yet is exactly what [`crate::mux::MovMuxer::check_bitstream`]'s
//! `extract_extradata` request exists to fix upstream, through the
//! `BsfProvider` seam `vaco_format_core::mux` already defines.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Result};
use vaco_format_isom::fourcc::FourCc;
use vaco_format_isom::{esds, writer};

/// One built sample entry, plus what the caller needs to finish the track.
#[derive(Debug, Clone)]
pub struct BuiltEntry {
    /// The framed `stsd` entry (e.g. one `avc1` box, extensions included).
    pub bytes: Vec<u8>,
    pub media: MediaType,
}

/// Build the one `stsd` entry for `params`.
///
/// # Errors
///
/// [`Error::Unsupported`] when the codec has no mapping this crate writes, or
/// the stream has no [`vaco_codec_core::VideoParameters`]/`AudioParameters`
/// for its declared media type.
pub fn build(params: &CodecParameters) -> Result<BuiltEntry> {
    let media = params
        .effective_media_type()
        .ok_or(Error::Unsupported("mp4: stream has no media type"))?;
    let codec = params
        .codec_id
        .ok_or(Error::Unsupported("mp4: stream has no codec id"))?;
    let extradata: &[u8] = params.extradata.as_deref().unwrap_or(&[]);

    match media {
        MediaType::Video => Ok(BuiltEntry {
            bytes: build_video(params, codec, extradata)?,
            media,
        }),
        MediaType::Audio => Ok(BuiltEntry {
            bytes: build_audio(params, codec, extradata)?,
            media,
        }),
        _ => Err(Error::Unsupported("mp4: only video and audio tracks")),
    }
}

/// Wrap an already-built sample entry for Common Encryption: rename the
/// entry's fourcc to `encv`/`enca` and append a `sinf` naming the original
/// format and the key id (ISO/IEC 23001-7 §8.3, `cenc` scheme, version-0
/// `tenc`, 8-byte per-sample IV — see
/// [`vaco_format_isom::writer::sinf_cenc`]).
#[must_use]
pub fn wrap_encrypted(entry: BuiltEntry, key_id: [u8; 16]) -> BuiltEntry {
    let mut bytes = entry.bytes;
    let mut original = *b"    ";
    if let Some(s) = bytes.get(4..8) {
        original.copy_from_slice(s);
    }
    let new_type: [u8; 4] = if entry.media == MediaType::Audio {
        *b"enca"
    } else {
        *b"encv"
    };
    if let Some(slot) = bytes.get_mut(4..8) {
        slot.copy_from_slice(&new_type);
    }
    let sinf = vaco_format_isom::writer::sinf_cenc(FourCc::new(&original), key_id);
    bytes.extend_from_slice(&sinf);
    let new_len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    if let Some(size_field) = bytes.get_mut(0..4) {
        size_field.copy_from_slice(&new_len.to_be_bytes());
    }
    BuiltEntry {
        bytes,
        media: entry.media,
    }
}

/// Whether `codec` needs out-of-band extradata to be muxed at all — the set
/// [`crate::mux::MovMuxer::check_bitstream`] asks `extract_extradata` for when
/// it is missing.
#[must_use]
pub fn needs_extradata(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::H264
            | CodecId::Hevc
            | CodecId::Av1
            | CodecId::Vp8
            | CodecId::Vp9
            | CodecId::Aac
            | CodecId::Alac
    )
}

fn build_video(params: &CodecParameters, codec: CodecId, extradata: &[u8]) -> Result<Vec<u8>> {
    let v = params.video.as_ref().ok_or(Error::Unsupported(
        "mp4: video stream has no VideoParameters",
    ))?;
    let width = u16::try_from(v.width).unwrap_or(u16::MAX);
    let height = u16::try_from(v.height).unwrap_or(u16::MAX);

    let mut extensions = Vec::new();
    let format = match codec {
        CodecId::H264 => {
            extensions.extend_from_slice(&writer::avcc(extradata));
            FourCc::new(b"avc1")
        }
        CodecId::Hevc => {
            extensions.extend_from_slice(&writer::hvcc(extradata));
            FourCc::new(b"hev1")
        }
        CodecId::Av1 => {
            extensions.extend_from_slice(&writer::av1c(extradata));
            FourCc::new(b"av01")
        }
        CodecId::Vp8 => {
            // Unlike VP9, VP8's colour fields live inside the boolean-coded
            // first partition (RFC 6386 §9.2) — genuinely unreachable
            // without running the arithmetic decoder, and real `ffmpeg
            // 9.0.1` does not mux VP8 into MP4 at all (measured: `-c copy`
            // on a real `libvpx`-encoded WebM refuses outright, "codec not
            // currently supported in container"). So there is no bitstream
            // filter that could ever hand this a real record the way
            // `vp9_extract_vpcc` does for VP9 — an empty `extradata` here
            // can only mean "nobody could ever supply one", not "not yet",
            // and a `vpcC` with a correct header and zero payload bytes is
            // a file real `ffprobe` refuses outright. Refuse by name
            // instead of writing it.
            if extradata.is_empty() {
                return Err(Error::Unsupported(
                    "mp4: vp8 has no vpcC configuration record and this build cannot derive one \
                     (its colour fields are only readable by decoding); refusing rather than \
                     writing an empty vpcC box",
                ));
            }
            extensions.extend_from_slice(&writer::vpcc(extradata));
            FourCc::new(b"vp08")
        }
        CodecId::Vp9 => {
            // Deliberately *not* refused here even when `extradata` is
            // empty, unlike VP8 just above: `WebM`/Matroska carries no
            // `CodecPrivate` for VP9 at all, so a stream copied straight
            // from there always calls this function once with none, before
            // `MovMuxer::check_bitstream`'s `vp9_extract_vpcc` request has
            // had a single packet to derive one from — `add_stream` runs
            // this immediately (`add_stream_with`), long before the first
            // `write_packet`. `MovMuxer::write_trailer` is the real gate:
            // it refuses the file if this track's extradata is *still*
            // empty once every packet has passed through, the same
            // "defer the check to the point nothing more can arrive"
            // shape `vaco-mux-matroska::mux::flush_header_bytes` already
            // uses for its own `requires_extradata_str` gate. Writing a
            // possibly-empty `vpcC` here is safe precisely because nothing
            // reads this box until `write_trailer` finishes rebuilding it
            // via `MovMuxer::adopt_new_extradata`.
            extensions.extend_from_slice(&writer::vpcc(extradata));
            FourCc::new(b"vp09")
        }
        // MJPEG/PNG-in-MP4 carry their (usually empty) configuration inside an
        // `esds`, per 14496-14 — there is no dedicated per-codec config box
        // for either. Measured against `vaco-format-isom::esds`'s own
        // `object_type_codec` table, which this must invert exactly for the
        // round trip to name the same codec back.
        CodecId::Jpeg => {
            extensions.extend_from_slice(&writer::esds(
                0x6C,
                esds::stream_type::VISUAL,
                0,
                0,
                extradata,
            ));
            FourCc::new(b"mp4v")
        }
        CodecId::Png => {
            extensions.extend_from_slice(&writer::esds(
                0x6D,
                esds::stream_type::VISUAL,
                0,
                0,
                extradata,
            ));
            FourCc::new(b"mp4v")
        }
        _ => {
            return Err(Error::Unsupported(
                "mp4: no sample-entry mapping for this video codec",
            ));
        }
    };

    // `pasp` whenever the aspect ratio is known, **including 1:1**. The
    // `num != den` guard here was the natural reading — a square-pixel `pasp`
    // says nothing a reader did not already assume — and it is not what the
    // reference does. Measured on `ffmpeg -c copy -f mp4` across three inputs:
    // a 1:1 source, a raw H.264 stream with no container SAR at all, and a
    // 16:11 source. All three got a `pasp`, carrying `1/1`, `1/1` and `16/11`
    // respectively.
    if v.sample_aspect_ratio.is_defined() && !v.sample_aspect_ratio.is_zero() {
        extensions.extend_from_slice(&writer::pasp(
            u32::try_from(v.sample_aspect_ratio.num).unwrap_or(1),
            u32::try_from(v.sample_aspect_ratio.den).unwrap_or(1),
        ));
    }

    Ok(writer::visual_sample_entry(&writer::VisualEntryFields {
        format,
        width,
        height,
        horiz_resolution: 72 << 16,
        vert_resolution: 72 << 16,
        depth: 0x0018,
        compressor: "",
        extensions: &extensions,
    }))
}

/// `dfLa`'s payload must be a real FLAC metadata block (a 4-byte header --
/// last-block flag, 7-bit type, 24-bit length -- then the payload), never
/// the bare `STREAMINFO`. `CodecParameters::extradata` for a FLAC stream is
/// `"fLaC" + STREAMINFO` (`FlacEncoder::extradata`'s own convention, the
/// same shape a standalone `.flac` file's magic-plus-first-block needs),
/// not a metadata block -- writing it into `dfLa` unstripped and unwrapped
/// left `ffmpeg` unable to even open the file: "STREAMINFO must be first
/// `FLACMetadataBlock`", because the bytes right after `dfLa`'s own
/// version+flags were `"fLaC"` where a real block header belongs. Measured
/// end to end: `vaco -i mono.wav -c:a flac out.m4a` produced a file
/// `ffmpeg -i out.m4a` refused outright. The same class of bug
/// `vaco-demux-mp4`'s `alac` extradata fix addressed, on the write side and
/// a different codec: extradata means something different per container,
/// and a muxer that writes it verbatim is trusting a shape nothing promised.
///
/// Deliberately re-implemented here rather than depending on
/// `vaco-codec-flac` for it: `vaco-mux-ogg::headers::streaminfo_payload_from_extradata`
/// already carries the identical "accept bare-34 or `fLaC`-wrapped" logic
/// as its own small, local copy rather than a cross-crate dependency on the
/// codec crate that happens to produce this shape, and this follows the
/// same precedent.
fn flac_streaminfo_metadata_block(extradata: &[u8]) -> Result<[u8; 38]> {
    let bad = || Error::Unsupported("mp4: FLAC extradata is not a recognised STREAMINFO shape");
    let payload: &[u8] = if extradata.len() == 34 {
        extradata
    } else {
        let body = extradata.strip_prefix(b"fLaC").ok_or_else(bad)?;
        let header = body.get(..4).ok_or_else(bad)?;
        let [b0, b1, b2, b3] = header else {
            return Err(bad());
        };
        if b0 & 0x7F != 0 {
            return Err(Error::Unsupported(
                "mp4: FLAC extradata's first metadata block is not STREAMINFO",
            ));
        }
        let len = (u32::from(*b1) << 16) | (u32::from(*b2) << 8) | u32::from(*b3);
        if len != 34 {
            return Err(bad());
        }
        body.get(4..38).ok_or_else(bad)?
    };
    let mut block = [0u8; 38];
    block[0] = 0x80; // last metadata block, type 0 (STREAMINFO)
    block[1] = 0;
    block[2] = 0;
    block[3] = 34;
    if let Some(dst) = block.get_mut(4..38) {
        dst.copy_from_slice(payload.get(..34).ok_or_else(bad)?);
    }
    Ok(block)
}

/// Convert an `OpusHead` blob (RFC 7845 §5.1: magic-prefixed, little-endian
/// fields — what `CodecParameters::extradata` carries for Opus everywhere in
/// this project, since Ogg and Matroska both store it verbatim) into the
/// body MP4's `dOps` box actually wants.
///
/// `dOps` is not "`OpusHead` minus its magic and version byte", a claim
/// `vaco-format-isom::writer::dops`'s doc comment used to make (and this
/// function's call site used to trust, passing `extradata` straight through
/// unconverted): it keeps the version byte and drops only the 8-byte magic,
/// and every multi-byte field is big-endian, not little. Measured against a
/// real `ffmpeg -c:a libopus -f mp4` fixture's own box: `00 02 01 38 00 00
/// bb 80 00 00 00` — version 0, channels 2, `pre_skip` 0x0138 = 312 (`ffprobe`
/// reports `initial_padding=312` for the same file), rate 0x0000bb80 =
/// 48000, gain 0, family 0. Without this conversion, every file this crate
/// muxed with `-c:a opus` carried a `dOps` box with a phantom 8-byte prefix
/// and every multi-byte field byte-swapped — silently wrong, not refused.
///
/// Deliberately re-implemented here rather than depending on
/// `vaco-parse-opus` (D14.1): the same precedent as
/// `flac_streaminfo_metadata_block` above, and the inverse of
/// `vaco-demux-mp4::track`'s own `dops_to_opus_head`.
fn opus_head_to_dops(extradata: &[u8]) -> Result<Vec<u8>> {
    let bad = || Error::Unsupported("mp4: Opus extradata is not a recognised OpusHead shape");
    let rest = extradata.strip_prefix(b"OpusHead").ok_or_else(bad)?;
    let pre_skip: [u8; 2] = rest
        .get(2..4)
        .ok_or_else(bad)?
        .try_into()
        .map_err(|_| bad())?;
    let rate: [u8; 4] = rest
        .get(4..8)
        .ok_or_else(bad)?
        .try_into()
        .map_err(|_| bad())?;
    let gain: [u8; 2] = rest
        .get(8..10)
        .ok_or_else(bad)?
        .try_into()
        .map_err(|_| bad())?;
    let mut out = Vec::new();
    out.push(*rest.first().ok_or_else(bad)?); // version
    out.push(*rest.get(1).ok_or_else(bad)?); // channel count
    out.extend_from_slice(&u16::from_le_bytes(pre_skip).to_be_bytes());
    out.extend_from_slice(&u32::from_le_bytes(rate).to_be_bytes());
    out.extend_from_slice(&u16::from_le_bytes(gain).to_be_bytes());
    out.extend_from_slice(rest.get(10..).ok_or_else(bad)?);
    Ok(out)
}

fn build_audio(params: &CodecParameters, codec: CodecId, extradata: &[u8]) -> Result<Vec<u8>> {
    let a = params.audio.as_ref().ok_or(Error::Unsupported(
        "mp4: audio stream has no AudioParameters",
    ))?;
    let channel_count = u16::try_from(a.layout.as_ref().map_or(1, |l| l.channels))
        .unwrap_or(1)
        .max(1);
    // The container's *stored* sample size, not the codec's: measured (see
    // source measurements, a compressed codec's MP4 sample entry
    // states 16 regardless of what the bitstream actually carries.
    let sample_size: u16 = 16;
    let rate_int = u16::try_from(a.sample_rate.min(0xFFFF)).unwrap_or(0xFFFF);
    let sample_rate_fp16 = u32::from(rate_int) << 16;

    let mut extensions = Vec::new();
    let format = match codec {
        CodecId::Aac => {
            extensions.extend_from_slice(&writer::esds(
                0x40,
                esds::stream_type::AUDIO,
                0,
                0,
                extradata,
            ));
            FourCc::new(b"mp4a")
        }
        CodecId::Opus => {
            extensions.extend_from_slice(&writer::dops(&opus_head_to_dops(extradata)?));
            FourCc::new(b"Opus")
        }
        CodecId::Flac => {
            extensions
                .extend_from_slice(&writer::dfla(&flac_streaminfo_metadata_block(extradata)?));
            FourCc::new(b"fLaC")
        }
        CodecId::Alac => {
            extensions.extend_from_slice(&writer::alac(extradata));
            FourCc::new(b"alac")
        }
        // No config box at all: `.mp3` in MP4 is self-describing (every frame
        // carries its own header), the same convention the reference uses.
        CodecId::Mp3 => FourCc::new(b".mp3"),
        _ => {
            return Err(Error::Unsupported(
                "mp4: no sample-entry mapping for this audio codec",
            ));
        }
    };

    Ok(writer::audio_sample_entry(&writer::AudioEntryFields {
        format,
        channel_count,
        sample_size,
        sample_rate_fp16,
        extensions: &extensions,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::opus_head_to_dops;

    /// The same real `ffmpeg -c:a libopus -f mp4` box `vaco-demux-mp4::track`
    /// tests against from the other direction: version 0, channels 2,
    /// `pre_skip` 0x0138 = 312, rate 0x0000bb80 = 48000, gain 0, family 0.
    const REAL_DOPS: [u8; 11] = [
        0x00, 0x02, 0x01, 0x38, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0x00,
    ];

    fn opus_head(pre_skip: u16, rate: u32) -> Vec<u8> {
        let mut h = b"OpusHead".to_vec();
        h.push(0); // version
        h.push(2); // channels
        h.extend_from_slice(&pre_skip.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // gain
        h.push(0); // mapping family
        h
    }

    #[test]
    fn opus_head_to_dops_matches_a_real_measured_box() {
        let dops = opus_head_to_dops(&opus_head(312, 48_000)).unwrap();
        assert_eq!(dops, REAL_DOPS);
    }

    #[test]
    fn opus_head_to_dops_rejects_anything_without_the_magic() {
        assert!(
            opus_head_to_dops(&REAL_DOPS).is_err(),
            "REAL_DOPS has no OpusHead magic"
        );
    }
}
