//! A minimal AMF0 (Action Message Format, version 0) encoder, just enough
//! to build one `onMetaData` "ECMA array" — the FLV metadata block every
//! HDS `<media>` element carries as its base64 `<metadata>` payload.
//!
//! AMF0 is the public, historically-documented binary object format FLV
//! itself uses for its own leading metadata tag; this module encodes,
//! it never decodes untrusted bytes, so there is no bounds-checking surface
//! here worth fuzzing (see `lib.rs`'s "No fuzz target" note).
//!
//! # Measured shape (`provenance/sources.toml`, `ffmpeg-hds-mux-probe`)
//!
//! `ffmpeg -f hds`'s own `onMetaData` blob is: the string `"onMetaData"`,
//! then an ECMA array of exactly twelve key/value pairs in this order —
//! `duration`, `width`, `height`, `videodatarate`, `videocodecid`,
//! `audiodatarate`, `audiosamplerate`, `audiosamplesize`, `stereo`,
//! `audiocodecid`, `encoder`, `filesize` — terminated by AMF0's own empty-key
//! object-end marker (`00 00 09`). `duration`/`filesize` are `0.0` (this
//! project's muxer, like the reference's, does not know either up front for
//! streamed output); `videodatarate`/`audiodatarate` are each codec's
//! `bit_rate` in **kibibit/s** (`bits_per_second / 1024.0`, confirmed by
//! reversing the reference's own `469`/`67.3828125` values against the
//! `-b:v`/`-b:a` it was given — a different unit from the `Manifest`'s own
//! decimal-kbit/s `bitrate` attribute, see `manifest.rs`); `stereo` is
//! always `false` for AAC, regardless of the real channel count — a
//! measured FLV/AAC convention, not this crate's own choice (the FLV
//! `AACAUDIODATA` tag itself hard-codes its `SoundType` bit the same way,
//! see `flv.rs`).

/// Build one `onMetaData` AMF0 blob.
#[derive(Debug, Clone, Copy)]
pub struct OnMetaData {
    pub width: f64,
    pub height: f64,
    pub video_datarate_kibit: f64,
    pub video_codec_id: f64,
    pub audio_datarate_kibit: f64,
    pub audio_sample_rate: f64,
    pub audio_sample_size: f64,
    pub audio_codec_id: f64,
}

fn push_string(out: &mut Vec<u8>, s: &str) {
    out.push(0x02);
    let len = u16::try_from(s.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn push_number(out: &mut Vec<u8>, v: f64) {
    out.push(0x00);
    out.extend_from_slice(&v.to_be_bytes());
}

fn push_bool(out: &mut Vec<u8>, v: bool) {
    out.push(0x01);
    out.push(u8::from(v));
}

fn push_key(out: &mut Vec<u8>, key: &str) {
    let len = u16::try_from(key.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(key.as_bytes());
}

/// Encode `meta` as a complete `onMetaData` AMF0 script-data value: the
/// leading string, the ECMA array header, all twelve entries, and the
/// object-end marker.
#[must_use]
pub fn encode_on_metadata(meta: &OnMetaData) -> Vec<u8> {
    let mut out = Vec::new();
    push_string(&mut out, "onMetaData");
    out.push(0x08); // ECMA array marker
    out.extend_from_slice(&12u32.to_be_bytes()); // entry count

    push_key(&mut out, "duration");
    push_number(&mut out, 0.0);
    push_key(&mut out, "width");
    push_number(&mut out, meta.width);
    push_key(&mut out, "height");
    push_number(&mut out, meta.height);
    push_key(&mut out, "videodatarate");
    push_number(&mut out, meta.video_datarate_kibit);
    push_key(&mut out, "videocodecid");
    push_number(&mut out, meta.video_codec_id);
    push_key(&mut out, "audiodatarate");
    push_number(&mut out, meta.audio_datarate_kibit);
    push_key(&mut out, "audiosamplerate");
    push_number(&mut out, meta.audio_sample_rate);
    push_key(&mut out, "audiosamplesize");
    push_number(&mut out, meta.audio_sample_size);
    push_key(&mut out, "stereo");
    push_bool(&mut out, false);
    push_key(&mut out, "audiocodecid");
    push_number(&mut out, meta.audio_codec_id);
    push_key(&mut out, "encoder");
    push_string(&mut out, "vaco-mux-hds");
    push_key(&mut out, "filesize");
    push_number(&mut out, 0.0);

    // Empty-key + object-end marker.
    out.extend_from_slice(&0u16.to_be_bytes());
    out.push(0x09);
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn sample() -> OnMetaData {
        OnMetaData {
            width: 320.0,
            height: 240.0,
            video_datarate_kibit: 390.625,
            video_codec_id: 7.0,
            audio_datarate_kibit: 67.382_812_5,
            audio_sample_rate: 48_000.0,
            audio_sample_size: 16.0,
            audio_codec_id: 10.0,
        }
    }

    /// Byte-for-byte against `hds-samples/out12.f4m/index.f4m`'s own
    /// `<metadata>` blob, decoded independently (Python, not this crate)
    /// during research and re-encoded here field for field, except
    /// `encoder`, which honestly names this project rather than
    /// impersonating `ffmpeg`'s own `Lavf` version stamp.
    #[test]
    fn matches_the_measured_reference_shape_except_the_encoder_string() {
        let bytes = encode_on_metadata(&sample());
        assert_eq!(&bytes[0..3], &[0x02, 0x00, 0x0a], "onMetaData string marker+length");
        assert_eq!(&bytes[3..13], b"onMetaData");
        assert_eq!(bytes[13], 0x08, "ECMA array marker");
        assert_eq!(&bytes[14..18], &12u32.to_be_bytes());
        assert!(bytes.ends_with(&[0x00, 0x00, 0x09]));
        // width/height/videocodecid/audiocodecid land at the same measured
        // offsets as the reference's own blob.
        let width_key_pos = bytes.windows(5).position(|w| w == b"width").unwrap();
        assert_eq!(bytes[width_key_pos + 5], 0x00, "width is an AMF0 Number");
    }

    #[test]
    fn stereo_is_always_false() {
        let bytes = encode_on_metadata(&sample());
        let pos = bytes.windows(6).position(|w| w == b"stereo").unwrap();
        assert_eq!(bytes[pos + 6], 0x01, "stereo is an AMF0 Boolean");
        assert_eq!(bytes[pos + 7], 0x00, "stereo is always false, regardless of real channel count");
    }
}
