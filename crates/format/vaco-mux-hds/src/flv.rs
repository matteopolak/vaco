//! FLV tag construction: this crate's actual fragment payload.
//!
//! **The reference does not use ISOBMFF fragments here at all** — measured
//! directly (`hds-samples/out12.f4m/stream0Seg1-Frag1`, box-walked): each
//! `Fragments`-equivalent file is one bare `mdat` box wrapping a sequence of
//! classic FLV tags (public format: Adobe's own FLV File Format
//! Specification), audio and video interleaved in arrival order, exactly
//! the same tag shape a `.flv` file itself uses. None of
//! `vaco-format-isom::writer`'s `mfhd`/`tfhd`/`trun`/`traf`/`moof` transfer
//! to this crate for that reason — only its generic, format-agnostic
//! `build::{bx, fullbx}` box-header helpers do (used here for the outer
//! `mdat`, and in `bootstrap.rs` for `abst`/`asrt`/`afrt`).
//!
//! # Tag layout (measured)
//!
//! ```text
//! TagType            u8        8 = audio, 9 = video
//! DataSize           u24 (BE)  byte length of the tag's own payload
//! Timestamp          u24 (BE)  low 24 bits, milliseconds
//! TimestampExtended  u8        high 8 bits of a 32-bit timestamp
//! StreamID           u24 (BE)  always 0
//! <payload>          DataSize bytes
//! PreviousTagSize    u32 (BE)  = 11 + DataSize (this tag's own total size)
//! ```
//!
//! Video payload (measured against the reference's own `avcC`-derived
//! sequence header and NALU tags): `FrameType(4 bits)<<4 | CodecID(4
//! bits=7, AVC)`, then `AVCPacketType` (`0` = sequence header, `1` = NALU),
//! then a 24-bit signed `CompositionTime` (milliseconds, PTS − DTS), then
//! either the raw `avcC` bytes verbatim (sequence header) or the sample's
//! own bytes **already length-prefixed the way `avcC`-configured MP4 stores
//! them** (`CodecParameters::nal_length_size == Some(4)` — this crate
//! requires that and refuses anything else, since re-framing Annex-B into
//! length-prefixed NALUs is out of scope here; see `lib.rs`).
//!
//! Audio payload: a fixed `0xAF` byte (`SoundFormat=10` AAC, `SoundRate`/
//! `SoundSize`/`SoundType` all fixed regardless of the real stream, a
//! measured FLV/AAC convention — the reference's own tag byte is `0xAF` for
//! a real 48 kHz **mono** stream), then `AACPacketType` (`0` = sequence
//! header, `1` = raw frame), then either the raw `AudioSpecificConfig`
//! bytes (sequence header) or the sample's own raw AAC access unit with no
//! ADTS framing (this crate assumes the same ADTS-free convention every
//! other MP4-family muxer in this workspace already relies on).

/// Append one complete FLV tag (header, payload, trailing `PreviousTagSize`)
/// to `out`.
pub fn write_tag(out: &mut Vec<u8>, tag_type: u8, timestamp_ms: u32, payload: &[u8]) {
    let data_size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.push(tag_type);
    out.extend_from_slice(&be24(data_size));
    out.extend_from_slice(&be24(timestamp_ms & 0x00ff_ffff));
    out.push(timestamp_extended(timestamp_ms));
    out.extend_from_slice(&[0, 0, 0]); // StreamID, always 0
    out.extend_from_slice(payload);
    let tag_total = 11u32.saturating_add(data_size);
    out.extend_from_slice(&tag_total.to_be_bytes());
}

/// The low 24 bits of a big-endian `u32`, as a 3-byte array — used for
/// `DataSize`/`Timestamp`, which FLV states as 24-bit fields.
fn be24(v: u32) -> [u8; 3] {
    let b = v.to_be_bytes();
    [b[1], b[2], b[3]]
}

/// The high 8 bits of a 32-bit millisecond timestamp (FLV's own
/// `TimestampExtended` field, needed once a stream runs past ~4.66 hours).
fn timestamp_extended(timestamp_ms: u32) -> u8 {
    let b = timestamp_ms.to_be_bytes();
    b[0]
}

/// `VIDEODATA`/`AVCVIDEOPACKET` payload (FLV video tag body).
#[must_use]
pub fn video_payload(
    is_key: bool,
    avc_packet_type: u8,
    composition_time_ms: i32,
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    let frame_type: u8 = if is_key { 1 } else { 2 };
    out.push((frame_type << 4) | 7); // CodecID 7 = AVC
    out.push(avc_packet_type);
    let ct = composition_time_ms.to_be_bytes();
    out.extend_from_slice(&[ct[1], ct[2], ct[3]]);
    out.extend_from_slice(body);
    out
}

/// `AUDIODATA`/`AACAUDIODATA` payload (FLV audio tag body). `SoundFormat`/
/// `SoundRate`/`SoundSize`/`SoundType` are fixed per the measured FLV/AAC
/// convention (see module docs) — real channel count and sample rate are
/// carried in `AudioSpecificConfig`/the `Manifest`'s own metadata instead.
#[must_use]
pub fn audio_payload(aac_packet_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0xAF);
    out.push(aac_packet_type);
    out.extend_from_slice(body);
    out
}

pub const AVC_PACKET_TYPE_SEQUENCE_HEADER: u8 = 0;
pub const AVC_PACKET_TYPE_NALU: u8 = 1;
pub const AAC_PACKET_TYPE_SEQUENCE_HEADER: u8 = 0;
pub const AAC_PACKET_TYPE_RAW: u8 = 1;

pub const TAG_TYPE_AUDIO: u8 = 8;
pub const TAG_TYPE_VIDEO: u8 = 9;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// The reference's own first video tag (sequence header) and first
    /// audio tag (sequence header), from `hds-samples/out12.f4m/
    /// stream0Seg1-Frag1`'s own measured mdat payload.
    #[test]
    fn video_sequence_header_matches_the_reference() {
        let avcc = vec![
            0x01, 0xf4, 0x00, 0x0d, 0xff, 0xe1, 0x00, 0x19, 0x67, 0xf4, 0x00, 0x0d, 0x91, 0x9b,
            0x28, 0x28,
        ];
        let payload = video_payload(true, AVC_PACKET_TYPE_SEQUENCE_HEADER, 0, &avcc);
        assert_eq!(payload.len(), 1 + 1 + 3 + avcc.len());
        let mut out = Vec::new();
        write_tag(&mut out, TAG_TYPE_VIDEO, 0, &payload);
        // TagType=9, DataSize=21=0x15, Timestamp=0, TimestampExt=0, StreamID=0.
        assert_eq!(&hex(&out)[0..8], "09000015");
        assert_eq!(&hex(&out)[8..16], "00000000");
        assert_eq!(&hex(&out)[16..22], "000000"); // StreamID
        assert_eq!(&hex(&out)[22..28], "170000"); // FrameType<<4|CodecID, AVCPacketType, CT high byte
        assert_eq!(
            out.len(),
            11 + payload.len() + 4,
            "header + payload + PreviousTagSize"
        );
    }

    #[test]
    fn audio_sequence_header_matches_the_reference() {
        let asc = vec![0x11, 0x88, 0x56, 0xe5, 0x00];
        let payload = audio_payload(AAC_PACKET_TYPE_SEQUENCE_HEADER, &asc);
        let mut out = Vec::new();
        write_tag(&mut out, TAG_TYPE_AUDIO, 0, &payload);
        // measured reference (`hds-samples/out12.f4m/stream0Seg1-Frag1`'s
        // own second tag): TagType=8, DataSize=7, Timestamp=0, StreamID=0,
        // payload `af 00 <AudioSpecificConfig>`, PreviousTagSize=18.
        assert_eq!(hex(&out), "0800000700000000000000af00118856e50000000012");
    }

    #[test]
    fn previous_tag_size_is_eleven_plus_payload_len() {
        let mut out = Vec::new();
        write_tag(&mut out, TAG_TYPE_AUDIO, 5, &[0xAA; 3]);
        assert_eq!(out.len(), 11 + 3 + 4);
        let prev_size = u32::from_be_bytes(out[out.len() - 4..].try_into().unwrap());
        assert_eq!(prev_size, 14);
    }

    #[test]
    fn timestamp_extended_carries_the_high_byte_past_the_24_bit_rollover() {
        let mut out = Vec::new();
        write_tag(&mut out, TAG_TYPE_VIDEO, 0x01_00_00_00, &[]);
        assert_eq!(out[7], 0x01, "TimestampExtended is the high byte");
        assert_eq!(&out[4..7], &[0, 0, 0], "low 24 bits rolled over to 0");
    }
}
