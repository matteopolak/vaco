//! Codec identity, reversed: [`CodecId`] to the `biCompression` `FourCC` or
//! `wFormatTag` this crate writes.
//!
//! The mirror of `vaco-demux-asf::header`'s read-side mapping (itself
//! bridging `vaco-format-asf::codec`), deliberately kept as its own small
//! table here rather than added to `vaco-format-asf` — the same "a reader's
//! parse state and a writer's serialise state are not one concept twice"
//! reasoning `vaco-mux-avi` already documents for its own `video_fourcc`/
//! `audio_format_tag`. Every mapping here is chosen so the *read* side
//! (`vaco_format_asf::codec`) resolves it back to the same [`CodecId`] —
//! writing a tag this workspace cannot itself demux again would be worse
//! than refusing the codec.

use vaco_codec_core::CodecId;
use vaco_format_riff::chunk::ChunkId;
use vaco_format_riff::wave::{
    WAVE_FORMAT_AAC, WAVE_FORMAT_ALAW, WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_MPEGLAYER3,
    WAVE_FORMAT_MULAW, WAVE_FORMAT_PCM, WAVE_FORMAT_WMAUDIO1, WAVE_FORMAT_WMAUDIO2,
};

/// See [`vaco_format_asf::codec::audio_codec_id`]'s `WAVE_FORMAT_WMAUDIO3`.
const WAVE_FORMAT_WMAUDIO3: u16 = 0x0162;

/// A video codec's `biCompression` `FourCC`, for the codecs this crate can
/// mux. `None` for anything `vaco_format_asf::codec::video_codec_id` would
/// not read back to the same [`CodecId`].
#[must_use]
pub fn video_fourcc(id: CodecId) -> Option<[u8; 4]> {
    match id {
        CodecId::H264 => Some(*b"H264"),
        CodecId::Hevc => Some(*b"HEVC"),
        CodecId::Vp8 => Some(*b"VP80"),
        CodecId::Vp9 => Some(*b"VP90"),
        CodecId::Jpeg => Some(*b"MJPG"),
        CodecId::Png => Some(*b"MPNG"),
        // ASF's own native video codec; `WVC1` (the advanced-profile
        // spelling) would round-trip identically, `WMV3` is the more
        // widely-recognised of the two this crate measured.
        CodecId::Vc1 => Some(*b"WMV3"),
        _ => None,
    }
}

/// An audio codec's `wFormatTag`, for the codecs this crate can mux. `None`
/// for anything `vaco_format_asf::codec::audio_codec_id` would not read
/// back to the same [`CodecId`].
#[must_use]
pub fn audio_format_tag(id: CodecId) -> Option<u16> {
    match id {
        CodecId::Pcm
        | CodecId::PcmU8
        | CodecId::PcmS16le
        | CodecId::PcmS24le
        | CodecId::PcmS32le => Some(WAVE_FORMAT_PCM),
        CodecId::PcmF32le | CodecId::PcmF64le => Some(WAVE_FORMAT_IEEE_FLOAT),
        CodecId::PcmAlaw => Some(WAVE_FORMAT_ALAW),
        CodecId::PcmMulaw => Some(WAVE_FORMAT_MULAW),
        CodecId::Mp3 => Some(WAVE_FORMAT_MPEGLAYER3),
        CodecId::Aac => Some(WAVE_FORMAT_AAC),
        CodecId::Wmav1 => Some(WAVE_FORMAT_WMAUDIO1),
        CodecId::Wmav2 => Some(WAVE_FORMAT_WMAUDIO2),
        CodecId::Wmapro => Some(WAVE_FORMAT_WMAUDIO3),
        _ => None,
    }
}

/// Whether `id` is an uncompressed PCM flavour — mirrors
/// `vaco-mux-avi::is_uncompressed_pcm`, needed here for the same reason: it
/// decides nothing about ASF packetisation directly (every ASF payload
/// carries an explicit size, unlike AVI's `dwSampleSize`), but a caller
/// choosing `nBlockAlign` for `WAVEFORMATEX` needs to know whether the
/// stream has one at all.
#[must_use]
pub const fn is_uncompressed_pcm(id: CodecId) -> bool {
    matches!(
        id,
        CodecId::Pcm
            | CodecId::PcmU8
            | CodecId::PcmS16le
            | CodecId::PcmS24le
            | CodecId::PcmS32le
            | CodecId::PcmF32le
            | CodecId::PcmF64le
            | CodecId::PcmAlaw
            | CodecId::PcmMulaw
    )
}

/// The `wBitsPerSample` a specific PCM flavour implies — mirrors
/// `vaco-mux-avi::pcm_bits_per_sample`; see that function's doc comment.
#[must_use]
pub const fn pcm_bits_per_sample(id: CodecId) -> Option<u16> {
    match id {
        CodecId::PcmU8 | CodecId::PcmAlaw | CodecId::PcmMulaw => Some(8),
        CodecId::PcmS16le => Some(16),
        CodecId::PcmS24le => Some(24),
        CodecId::PcmS32le | CodecId::PcmF32le => Some(32),
        CodecId::PcmF64le => Some(64),
        _ => None,
    }
}

/// Build a `Compression::FourCc`-shaped raw `u32` the way ASF's `Compression
/// ID` field states it: "the first character of the four-character code
/// appears as the least-significant byte" ([\[ASF\] §9.2](vaco_format_asf)),
/// i.e. the bytes read in file order, reinterpreted as a little-endian
/// `u32` — the identical convention `vaco-format-riff::bitmapinfo::Compression`
/// already uses for `biCompression`.
#[must_use]
pub fn fourcc_to_u32(bytes: [u8; 4]) -> u32 {
    u32::from_le_bytes(bytes)
}

/// A `wFormatTag` reinterpreted as a four-byte tag the way `codec_tag`
/// prints it, mirroring `vaco-mux-avi::FormatTagBytes`.
#[must_use]
pub fn format_tag_bytes(tag: u16) -> [u8; 4] {
    let b = tag.to_le_bytes();
    [b[0], b[1], 0, 0]
}

/// Re-export so callers building `Compression::FourCc` values for tests do
/// not need a second `vaco-format-riff` import path.
#[must_use]
pub fn chunk_id(bytes: [u8; 4]) -> ChunkId {
    ChunkId::new(&bytes)
}

#[cfg(test)]
#[allow(clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn video_and_audio_mappings_round_trip_through_the_read_side() {
        for id in [
            CodecId::H264,
            CodecId::Hevc,
            CodecId::Vp8,
            CodecId::Vp9,
            CodecId::Jpeg,
            CodecId::Png,
            CodecId::Vc1,
        ] {
            let Some(fourcc) = video_fourcc(id) else {
                panic!("{id:?} should have a FourCC")
            };
            let compression = vaco_format_riff::bitmapinfo::Compression::FourCc(chunk_id(fourcc));
            assert_eq!(
                vaco_format_asf::codec::video_codec_id(compression),
                Some(id)
            );
        }
    }

    #[test]
    fn a_codec_with_no_mapping_is_none() {
        assert_eq!(video_fourcc(CodecId::Av1), None);
        assert_eq!(audio_format_tag(CodecId::Flac), None);
    }
}
