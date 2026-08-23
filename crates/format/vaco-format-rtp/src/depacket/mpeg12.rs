//! RFC 2250: MPEG-1/2 video (`MPV`, RFC 3551 PT 32) and MPEG audio (`MPA`,
//! PT 14) over RTP.
//!
//! Both share the shape "a small fixed header, then a slice of the
//! elementary stream verbatim" — RFC 2250 §3.4 (audio, 4-byte header) and
//! §3.5 (video, also a 4-byte "MPEG video-specific header" per §2, plus an
//! optional 4-byte MPEG-2-only extension when `T` is set).

use vaco_core::{Error, Result};

use super::Depacketizer;

/// RFC 2250 §3.4: MPEG audio. The 4-byte header is `MBZ`(16 bits, must be
/// zero) + `Frag_offset`(16 bits, fragment offset of this packet's data
/// within the current ADU) — this module supports only `Frag_offset == 0`
/// with the whole frame in one packet, which is what every encoder in this
/// workspace's test corpus emits; a genuinely fragmented ADU is reported as
/// [`Error::Unsupported`].
#[derive(Debug, Default)]
pub struct Mpa;

impl Depacketizer for Mpa {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 4] =
            payload
                .get(0..4)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP MPA payload shorter than its 4-byte header",
                ))?;
        let frag_offset = u16::from_be_bytes([header[2], header[3]]);
        if frag_offset != 0 {
            return Err(Error::Unsupported(
                "fragmented RFC 2250 MPA payloads are not implemented",
            ));
        }
        let body = payload.get(4..).ok_or(Error::InvalidData(
            "RTP MPA payload has no frame after its header",
        ))?;
        Ok(Some(body.to_vec()))
    }
}

/// RFC 2250 §2 "MPEG video-specific header":
/// `MBZ`(13)+`T`(1)+`TR`(10)+`AN`(1)+`N`(1)+`S`(1)+`B`(1)+`E`(1)+`P`(3)+`FBV`(1)+`BFC`(3)+`FFV`(1)+`FFC`(3),
/// 4 bytes, plus a further 4-byte MPEG-2 extension header when `T` is set.
/// This module strips whichever header is present and hands back the
/// elementary-stream slice unmodified — reassembly across packets is not
/// needed because RFC 2250 video packets already align on ES byte
/// boundaries and the demuxer's own frame boundary detection (start codes)
/// does the rest, exactly as it would for a raw `.m2v` file.
#[derive(Debug, Default)]
pub struct Mpv;

impl Depacketizer for Mpv {
    fn push(&mut self, _marker: bool, _timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let header: [u8; 4] =
            payload
                .get(0..4)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData(
                    "RTP MPV payload shorter than its 4-byte header",
                ))?;
        let has_mpeg2_ext = header[0] & 0x08 != 0; // `T` bit
        let skip = if has_mpeg2_ext { 8 } else { 4 };
        let body = payload.get(skip..).ok_or(Error::InvalidData(
            "RTP MPV payload has no data after its header",
        ))?;
        Ok(Some(body.to_vec()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn mpa_strips_four_byte_header() {
        let mut d = Mpa;
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(b"mp2-frame");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"mp2-frame".to_vec())
        );
    }

    #[test]
    fn mpa_rejects_fragmented() {
        let mut d = Mpa;
        let payload = vec![0, 0, 0, 1];
        assert!(d.push(true, 0, &payload).is_err());
    }

    #[test]
    fn mpv_strips_four_byte_header_without_extension() {
        let mut d = Mpv;
        let mut payload = vec![0x00, 0, 0, 0];
        payload.extend_from_slice(b"es-bytes");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"es-bytes".to_vec())
        );
    }

    #[test]
    fn mpv_strips_eight_bytes_when_mpeg2_extension_present() {
        let mut d = Mpv;
        let mut payload = vec![0x08, 0, 0, 0, 0, 0, 0, 0];
        payload.extend_from_slice(b"es-bytes");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"es-bytes".to_vec())
        );
    }
}
