//! Media packet preambles (SMPTE 360-2009 clause 7.4.2.1, Table 18): the
//! fixed 16-byte header preceding every media packet's essence bytes.
//!
//! Field widths and byte order here are as measured against the real
//! fixture and cross-checked against the Standard: every value is
//! big-endian ("most significant byte first", the same clause 7.1.2.2
//! exception `map.rs` documents — media preambles restate it explicitly in
//! clause 7.4.2.1).

use vaco_core::{Error, Result};

/// One media packet's preamble, always exactly 16 bytes.
#[derive(Debug, Clone, Copy)]
pub struct MediaPreamble {
    /// Table 5 media type of the essence that follows.
    pub media_type: u8,
    /// Index into the MAP packet's own track description vector — *not*
    /// the same number space as [`crate::map::TrackDescription::track_id`]
    /// after its `+0xC0` bias is removed, though in every file this crate
    /// has measured the two coincide (clause 7.4.2.1.2: "Track descriptions
    /// shall be considered as consecutive elements of a vector and track
    /// numbers are the index into that vector").
    pub track_number: u8,
    /// Field location within the *current media file* (clause 7.4.2.1.3) —
    /// only the same as [`MediaPreamble::timeline_field_number`] for a
    /// simple clip (see the crate's top-level docs on compound clips).
    pub media_field_number: u32,
    /// Raw, media-type-dependent (clause 7.4.2.1.4): a 4096-byte block
    /// count for Motion JPEG/DV, an MPEG picture-coding/structure nibble
    /// pair for MPEG (see [`MediaPreamble::mpeg_frame_info`]), or a
    /// first/last valid sample pair for audio and time code.
    pub field_info: [u8; 4],
    /// Field location on the composition's own timeline (clause
    /// 7.4.2.1.5) — valid only when [`MediaPreamble::timeline_field_valid`]
    /// is `true`.
    pub timeline_field_number: u32,
    flags: u8,
}

impl MediaPreamble {
    /// Parse the 16 bytes immediately following a `MEDIA` packet's own
    /// 16-byte [`crate::packet::PacketHeader`].
    ///
    /// # Errors
    /// [`Error::InvalidData`] if fewer than 16 bytes are given.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let b: &[u8; 16] =
            bytes
                .get(..16)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "gxf: media packet preamble is shorter than 16 bytes",
                ))?;
        Ok(Self {
            media_type: b[0],
            track_number: b[1],
            media_field_number: u32::from_be_bytes([b[2], b[3], b[4], b[5]]),
            field_info: [b[6], b[7], b[8], b[9]],
            timeline_field_number: u32::from_be_bytes([b[10], b[11], b[12], b[13]]),
            flags: b[14],
            // b[15] is reserved.
        })
    }

    /// Clause 7.4.2.1.6: bit 0 of the flags byte. When `false`, a reader
    /// shall treat [`MediaPreamble::timeline_field_number`] as equal to
    /// [`MediaPreamble::media_field_number`] instead of trusting the
    /// (possibly stale or absent) value on the wire.
    #[must_use]
    pub const fn timeline_field_valid(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// The field position to use for this packet's `pts`/`dts`: the
    /// composition-timeline field number when valid, else the media file's
    /// own (clause 7.4.2.1.6's own fallback rule, applied once here rather
    /// than at every call site).
    #[must_use]
    pub const fn effective_field_number(&self) -> u32 {
        if self.timeline_field_valid() {
            self.timeline_field_number
        } else {
            self.media_field_number
        }
    }

    /// For an MPEG media type only (Table 5 types 11, 12, 20, 22, 23):
    /// `field_info`'s Table 19 interpretation. Table 19 numbers its bits
    /// "0 is LSB", so picture coding is the low two bits of `field_info[0]`
    /// and picture structure the next two — not the high bits, which is
    /// the mistake a first pass at this made and the real fixture's own
    /// first packet (`field_info[0] == 0x0D`, a real I-frame per
    /// `ffprobe`) caught.
    #[must_use]
    pub const fn mpeg_frame_info(&self) -> MpegFrameInfo {
        let b0 = self.field_info[0];
        MpegFrameInfo {
            picture_coding: b0 & 0b11,
            picture_structure: (b0 >> 2) & 0b11,
        }
    }
}

/// Table 19: MPEG video frame descriptive information, decoded from a
/// media preamble's `field_info[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpegFrameInfo {
    picture_coding: u8,
    picture_structure: u8,
}

impl MpegFrameInfo {
    /// `true` for an I-frame (Table 19: `01`) — the only picture coding
    /// this crate needs to distinguish, since it is what
    /// [`vaco_packet::PacketFlags::KEY`] means.
    #[must_use]
    pub const fn is_intra(&self) -> bool {
        self.picture_coding == 0b01
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// The real fixture's first video MEDIA packet preamble (offset
    /// 0x113a4 + 16, past that packet's own header) — `media_type=12`,
    /// track=0, `field_number=0`, an I-frame, flags=0x01 (timeline field
    /// valid). Bytes transcribed once from
    /// `tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf`.
    const FIRST_VIDEO_PREAMBLE: [u8; 16] = [
        0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x92, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x00,
    ];

    #[test]
    fn parses_the_real_fixtures_first_video_preamble() {
        let p = MediaPreamble::parse(&FIRST_VIDEO_PREAMBLE).unwrap();
        assert_eq!(p.media_type, 12);
        assert_eq!(p.track_number, 0);
        assert_eq!(p.media_field_number, 0);
        assert_eq!(p.timeline_field_number, 0);
        assert!(p.timeline_field_valid());
        assert_eq!(p.effective_field_number(), 0);
        assert!(p.mpeg_frame_info().is_intra());
    }

    #[test]
    fn falls_back_to_the_media_field_number_when_the_timeline_one_is_invalid() {
        let mut bytes = FIRST_VIDEO_PREAMBLE;
        bytes[2..6].copy_from_slice(&7u32.to_be_bytes());
        bytes[10..14].copy_from_slice(&99u32.to_be_bytes());
        bytes[14] = 0x00; // timeline field invalid
        let p = MediaPreamble::parse(&bytes).unwrap();
        assert_eq!(p.effective_field_number(), 7);
    }

    #[test]
    fn a_short_slice_is_invalid_data_not_a_panic() {
        assert!(MediaPreamble::parse(&FIRST_VIDEO_PREAMBLE[..15]).is_err());
    }
}
