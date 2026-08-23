//! Resolve an SDP `a=rtpmap` encoding name (or an RFC 3551 static payload
//! type) to the depacketiser that understands it.
//!
//! Not a `vaco-registry` registration — see this crate's docs on why a
//! `vaco-format-*` crate never depends on `vaco-registry` (D14.1). This is a
//! plain lookup table `vaco-demux-rtsp` calls directly.

use vaco_codec_core::CodecId;
use vaco_core::Result;

use super::{
    Depacketizer, aac, av1, h263, h264, hevc, jpeg, mpeg12, raw, rawvideo, red, vp8, vp9, xiph,
};

/// A depacketiser constructor. A plain `fn` pointer rather than a `dyn Fn`,
/// since every depacketiser here is a bare `T::default()` — the one
/// exception, RFC 2198 `red`, is handled separately by [`red_wrapping`]
/// because it needs the *primary* encoding's own depacketiser as an input,
/// not just a codec id.
pub type DepacketizerFactory = fn() -> Box<dyn Depacketizer>;

/// Resolve an `a=rtpmap` encoding name (matched case-insensitively, as
/// every RTSP server this crate was checked against sends a mix of
/// `H264`/`h264`) to the [`CodecId`] it decodes to and a depacketiser
/// constructor for it.
///
/// Returns `None` for `red` (see [`red_wrapping`]) and for any encoding
/// name this crate does not implement — see `crate::depacket`'s module
/// docs for the full list of what that excludes and why.
#[must_use]
pub fn for_encoding(name: &str) -> Option<(CodecId, DepacketizerFactory)> {
    let upper = name.to_ascii_uppercase();
    let entry: (CodecId, DepacketizerFactory) = match upper.as_str() {
        "PCMU" => (CodecId::PcmMulaw, || Box::new(raw::Identity)),
        "PCMA" => (CodecId::PcmAlaw, || Box::new(raw::Identity)),
        "L16" => (CodecId::PcmS16be, || Box::new(raw::Identity)),
        "OPUS" => (CodecId::Opus, || Box::new(raw::Identity)),
        "SPEEX" => (CodecId::Speex, || Box::new(raw::Identity)),
        "AMR" => (CodecId::AmrNb, || Box::new(raw::Identity)),
        "AMR-WB" => (CodecId::AmrWb, || Box::new(raw::Identity)),
        "AC3" => (CodecId::Ac3, || Box::new(raw::Ac3)),
        "MPA" => (CodecId::Mp2, || Box::new(mpeg12::Mpa)),
        "MPV" => (CodecId::Mpeg1video, || Box::new(mpeg12::Mpv)),
        "JPEG" => (
            CodecId::Jpeg,
            || Box::new(jpeg::JpegDepacketizer::default()),
        ),
        "H261" => (CodecId::H261, || Box::new(raw::Identity)),
        "H263-1998" | "H263-2000" => (
            CodecId::H263,
            || Box::new(h263::H263Depacketizer::default()),
        ),
        "H264" => (
            CodecId::H264,
            || Box::new(h264::H264Depacketizer::default()),
        ),
        "H265" => (
            CodecId::Hevc,
            || Box::new(hevc::HevcDepacketizer::default()),
        ),
        "VP8" => (CodecId::Vp8, || Box::new(vp8::Vp8Depacketizer::default())),
        "VP9" => (CodecId::Vp9, || Box::new(vp9::Vp9Depacketizer::default())),
        "AV1" => (CodecId::Av1, || Box::new(av1::Av1Depacketizer::default())),
        "MPEG4-GENERIC" => (CodecId::Aac, || Box::new(aac::AacDepacketizer::default())),
        "VORBIS" => (CodecId::Vorbis, || {
            Box::new(xiph::XiphDepacketizer::default())
        }),
        "THEORA" => (CodecId::Theora, || {
            Box::new(xiph::XiphDepacketizer::default())
        }),
        "RAW" => (CodecId::Rawvideo, || {
            Box::new(rawvideo::RawVideoDepacketizer::default())
        }),
        _ => return None,
    };
    Some(entry)
}

/// Wrap `primary` (the depacketiser for the encoding named by RED's own
/// `a=fmtp` primary payload type) so RFC 2198 redundancy blocks are
/// stripped before reaching it.
#[must_use]
pub fn red_wrapping(primary: Box<dyn Depacketizer>) -> Box<dyn Depacketizer> {
    Box::new(red::RedDepacketizer::new(primary))
}

/// RFC 2250 §2: `MP2T` (RTP static PT 33) carries whole, back-to-back
/// 188-byte MPEG-2 TS packets with **no RTP-level framing of its own** — the
/// entire RTP payload (after the 12-byte RTP header, already stripped by
/// [`crate::rtp::RtpPacket::parse`]) is TS packets verbatim. This function
/// exists mainly to document that fact and to reject a payload that is not
/// TS-packet-aligned, which is the one thing worth checking before handing
/// bytes to a nested `vaco-demux-mpegts` — the composition itself is
/// `vaco-demux-rtsp`'s job (see that crate's docs for the deferred gap).
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if `payload`'s length is not a
/// multiple of 188 or does not start with TS's `0x47` sync byte.
pub fn mp2t_payload(payload: &[u8]) -> Result<&[u8]> {
    if !payload.len().is_multiple_of(188) || payload.len() < 188 {
        return Err(vaco_core::Error::InvalidData(
            "RTP MP2T payload is not a whole number of 188-byte TS packets",
        ));
    }
    let first = *payload
        .first()
        .ok_or(vaco_core::Error::InvalidData("RTP MP2T payload is empty"))?;
    if first != 0x47 {
        return Err(vaco_core::Error::InvalidData(
            "RTP MP2T payload does not start with the TS sync byte",
        ));
    }
    Ok(payload)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn resolves_case_insensitively() {
        assert_eq!(for_encoding("h264").unwrap().0, CodecId::H264);
        assert_eq!(for_encoding("H264").unwrap().0, CodecId::H264);
    }

    #[test]
    fn unknown_encoding_is_none() {
        assert!(for_encoding("GSM").is_none());
        assert!(for_encoding("red").is_none());
    }

    #[test]
    fn mp2t_payload_accepts_aligned_ts_packets() {
        let mut buf = vec![0x47u8];
        buf.resize(188, 0);
        assert!(mp2t_payload(&buf).is_ok());
    }

    #[test]
    fn mp2t_payload_rejects_misaligned_length() {
        assert!(mp2t_payload(&[0x47; 100]).is_err());
    }
}
