//! A best-effort `picture_coding_type` sniff for MPEG-1/2 video, used only to
//! set [`vaco_packet::PacketFlags::KEY`].
//!
//! This is a structural fact about ISO/IEC 11172-2 / ISO/IEC 13818-2's
//! picture header (a fixed bit layout the format dictates, not an
//! implementation detail), read directly from bytes already in hand — it
//! does not need a real MPEG video parser, and this crate carries no
//! dependency on one (D14.1: no `vaco-parse-*` crate; program-stream video
//! is virtually always MPEG-1/2, for which no parser exists in this
//! workspace yet — see the docs file).
//!
//! Approximate by design: a PES payload boundary need not align with a
//! picture start code (a single PES packet can span more than one picture,
//! or a picture can span more than one PES packet), so this only answers
//! "does a picture header appear in this payload, and if so what type is
//! it" — good enough to flag the common case (one picture per PES packet,
//! which is what every sample this was measured against does) but not a
//! substitute for real parsing.

/// `picture_start_code`.
const PICTURE_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0x00];

/// Bytes scanned looking for a picture start code, bounding the cost on a
/// payload that never contains one.
const SCAN_LIMIT: usize = 4096;

/// Whether the first picture header found in `payload` declares
/// `picture_coding_type == 1` (I-frame). `None` when no picture header is
/// found within [`SCAN_LIMIT`] bytes.
#[must_use]
pub fn is_keyframe(payload: &[u8]) -> Option<bool> {
    let window = payload.get(..payload.len().min(SCAN_LIMIT))?;
    let at = window.windows(4).position(|w| w == PICTURE_START_CODE)?;
    // Immediately after the 4-byte start code: temporal_reference (10 bits),
    // then picture_coding_type (3 bits). The type lives in the top 3 bits of
    // the second byte after the start code.
    let b1 = *payload.get(at.checked_add(5)?)?;
    Some((b1 >> 3) & 0x07 == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture_header(coding_type: u8) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x01, 0x00];
        // temporal_reference = 0, picture_coding_type in top 3 bits of the
        // second byte.
        v.push(0x00);
        v.push((coding_type & 0x07) << 3);
        v.push(0x00);
        v
    }

    #[test]
    fn an_i_frame_is_detected() {
        assert_eq!(is_keyframe(&picture_header(1)), Some(true));
    }

    #[test]
    fn a_p_frame_is_not_a_keyframe() {
        assert_eq!(is_keyframe(&picture_header(2)), Some(false));
    }

    #[test]
    fn no_picture_header_yields_none() {
        assert_eq!(is_keyframe(b"not a picture"), None);
    }

    #[test]
    fn an_empty_payload_yields_none() {
        assert_eq!(is_keyframe(&[]), None);
    }
}
