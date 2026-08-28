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

/// Whether `codec` needs out-of-band extradata to be muxed at all — the set
/// [`crate::mux::MovMuxer::check_bitstream`] asks `extract_extradata` for when
/// it is missing.
#[must_use]
pub fn needs_extradata(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::H264 | CodecId::Hevc | CodecId::Av1 | CodecId::Vp8 | CodecId::Vp9 | CodecId::Aac
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
            extensions.extend_from_slice(&writer::vpcc(extradata));
            FourCc::new(b"vp08")
        }
        CodecId::Vp9 => {
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
    // respectively (CONFORMANCE-FINDINGS 36).
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

fn build_audio(params: &CodecParameters, codec: CodecId, extradata: &[u8]) -> Result<Vec<u8>> {
    let a = params.audio.as_ref().ok_or(Error::Unsupported(
        "mp4: audio stream has no AudioParameters",
    ))?;
    let channel_count = u16::try_from(a.layout.as_ref().map_or(1, |l| l.channels))
        .unwrap_or(1)
        .max(1);
    // The container's *stored* sample size, not the codec's: measured (see
    // `planning/AGENT-CONSTRAINTS.md`), a compressed codec's MP4 sample entry
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
            extensions.extend_from_slice(&writer::dops(extradata));
            FourCc::new(b"Opus")
        }
        CodecId::Flac => {
            extensions.extend_from_slice(&writer::dfla(extradata));
            FourCc::new(b"fLaC")
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
