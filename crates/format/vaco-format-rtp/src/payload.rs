//! RFC 3551 §6 static RTP/AVP payload-type assignments.
//!
//! This table is the "RTP payload registry" named as one of the two
//! authorities for FM-41's depacketiser count (the other, `ffmpeg -h
//! demuxer=rtp`, turns out not to enumerate them at all — see
//! `vaco-demux-rtsp`'s crate docs for that finding). It is a public IANA/IETF
//! registry, not `FFmpeg` source, so citing it is unambiguously clean-room
//! (D7).
//!
//! Every row is cited to RFC 3551 Table 4 (audio) and Table 5 (video) —
//! `provenance/vaco-format-rtp.toml`. A dynamic payload type (`96..=127`, or
//! any number an `a=rtpmap` line reassigns) is not in this table at all:
//! [`crate::sdp`] resolves those from the session description instead.

use vaco_codec_core::CodecId;

/// One row of the static payload-type table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPayload {
    pub payload_type: u8,
    /// The `a=rtpmap` encoding name this payload type is equivalent to.
    pub name: &'static str,
    pub clock_rate: u32,
    pub channels: u8,
    /// `None` for a payload type this workspace has no matching
    /// [`CodecId`] for yet — see `vaco-demux-rtsp`'s report for the list of
    /// `CodecId` variants this crate would need `vaco-codec-core` to add.
    pub codec: Option<CodecId>,
}

macro_rules! row {
    ($pt:expr, $name:expr, $rate:expr, $ch:expr, $codec:expr) => {
        StaticPayload {
            payload_type: $pt,
            name: $name,
            clock_rate: $rate,
            channels: $ch,
            codec: $codec,
        }
    };
}

/// RFC 3551 Tables 4 (audio, PT 0..=23) and 5 (video, PT 24..=34), in
/// payload-type order — **every** row either table lists, including the
/// ones the RFC itself marks `reserved` or `unassigned` rather than only
/// the ones with a real encoding name. 35 rows: this is the table the
/// brief predicted would cross the provenance-check threshold, and,
/// measured by actually transcribing both tables rather than only their
/// assigned rows, it does — see `provenance/vaco-format-rtp.toml`.
pub const STATIC_PAYLOADS: &[StaticPayload] = &[
    row!(0, "PCMU", 8000, 1, Some(CodecId::PcmMulaw)),
    row!(1, "reserved", 0, 0, None),
    row!(2, "reserved", 0, 0, None),
    row!(3, "GSM", 8000, 1, None),
    row!(4, "G723", 8000, 1, None),
    row!(5, "DVI4", 8000, 1, None),
    row!(6, "DVI4", 16000, 1, None),
    row!(7, "LPC", 8000, 1, None),
    row!(8, "PCMA", 8000, 1, Some(CodecId::PcmAlaw)),
    row!(9, "G722", 8000, 1, None),
    row!(10, "L16", 44100, 2, Some(CodecId::PcmS16be)),
    row!(11, "L16", 44100, 1, Some(CodecId::PcmS16be)),
    row!(12, "QCELP", 8000, 1, None),
    row!(13, "CN", 8000, 1, None),
    row!(14, "MPA", 90000, 0, Some(CodecId::Mp2)),
    row!(15, "G728", 8000, 1, None),
    row!(16, "DVI4", 11025, 1, None),
    row!(17, "DVI4", 22050, 1, None),
    row!(18, "G729", 8000, 1, None),
    row!(19, "reserved", 0, 0, None),
    row!(20, "unassigned", 0, 0, None),
    row!(21, "unassigned", 0, 0, None),
    row!(22, "unassigned", 0, 0, None),
    row!(23, "unassigned", 0, 0, None),
    row!(24, "unassigned", 0, 0, None),
    row!(25, "CelB", 90000, 0, None),
    row!(26, "JPEG", 90000, 0, Some(CodecId::Jpeg)),
    row!(27, "unassigned", 0, 0, None),
    row!(28, "nv", 90000, 0, Some(CodecId::Rawvideo)),
    row!(29, "unassigned", 0, 0, None),
    row!(30, "unassigned", 0, 0, None),
    row!(31, "H261", 90000, 0, Some(CodecId::H261)),
    row!(32, "MPV", 90000, 0, Some(CodecId::Mpeg1video)),
    row!(33, "MP2T", 90000, 0, None),
    row!(34, "H263", 90000, 0, Some(CodecId::H263)),
];

/// Look up a payload-type row by its wire number.
///
/// Returns `None` outside `0..=34` entirely (dynamic `96..=127` and the
/// unlisted `35..=95` range have no table row at all — callers must fall
/// back to `a=rtpmap` for those, per RFC 3551 §3). Inside `0..=34` this
/// always returns `Some`, including for the RFC's own `reserved`/
/// `unassigned` rows; check [`StaticPayload::codec`] for whether the row
/// actually names an implementable codec.
#[must_use]
pub fn static_payload(pt: u8) -> Option<&'static StaticPayload> {
    STATIC_PAYLOADS.iter().find(|row| row.payload_type == pt)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn known_static_types_resolve() {
        assert_eq!(static_payload(0).unwrap().name, "PCMU");
        assert_eq!(static_payload(33).unwrap().name, "MP2T");
        assert_eq!(static_payload(96), None);
        assert_eq!(static_payload(255), None);
    }

    #[test]
    fn table_is_sorted_by_payload_type() {
        let mut prev = None;
        for row in STATIC_PAYLOADS {
            if let Some(p) = prev {
                assert!(row.payload_type > p, "table is not strictly increasing");
            }
            prev = Some(row.payload_type);
        }
    }
}
