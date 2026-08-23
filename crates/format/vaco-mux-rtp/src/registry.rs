//! [`Packetizer`] and the [`CodecId`]-keyed lookup for one.

use vaco_codec_core::CodecId;

/// Turns one coded access unit into a sequence of RTP payloads (the
/// packetiser's own header included, if the payload format has one — the
/// RTP header itself is added by [`crate::muxer::RtpMuxer`], not here).
///
/// The mirror image of `vaco_format_rtp::Depacketizer`; the two are not the
/// same trait because a depacketiser consumes payloads one at a time and
/// may hold reassembly state, while a packetiser is handed one whole access
/// unit and must decide *how many* pieces to split it into right away —
/// different enough shapes that forcing one trait over both would make at
/// least one side awkward.
pub trait Packetizer: Send {
    /// Split `au` into RTP payloads no larger than `mtu` bytes each (the
    /// packetiser's own per-payload header counts against this budget).
    /// The caller sets the RTP marker bit on the last payload returned and
    /// timestamps every payload with the access unit's own timestamp — RFC
    /// 3550 §5.1 says the marker/timestamp are the same for every packet of
    /// one frame, so neither is this trait's job to decide per-payload.
    fn packetize(&mut self, au: &[u8], mtu: usize) -> Vec<Vec<u8>>;
}

pub type PacketizerFactory = fn() -> Box<dyn Packetizer>;

/// Resolve a [`CodecId`] to its RTP payload-type name (for `a=rtpmap`) and a
/// packetiser constructor. Mirrors `vaco_format_rtp::depacket::registry::for_encoding`'s
/// coverage — see that module's docs for exactly which codecs and why.
#[must_use]
pub fn packetizer_for(codec: CodecId) -> Option<(&'static str, PacketizerFactory)> {
    let entry: (&'static str, PacketizerFactory) = match codec {
        CodecId::PcmMulaw => ("PCMU", || Box::new(crate::raw::RawPacketizer)),
        CodecId::PcmAlaw => ("PCMA", || Box::new(crate::raw::RawPacketizer)),
        CodecId::PcmS16be => ("L16", || Box::new(crate::raw::RawPacketizer)),
        CodecId::Opus => ("opus", || Box::new(crate::raw::RawPacketizer)),
        CodecId::H264 => ("H264", || Box::new(crate::h264::H264Packetizer)),
        CodecId::Aac => ("MPEG4-GENERIC", || {
            Box::new(crate::aac::AacPacketizer::default())
        }),
        _ => return None,
    };
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_codecs() {
        assert!(packetizer_for(CodecId::H264).is_some());
        assert!(packetizer_for(CodecId::Opus).is_some());
    }

    #[test]
    fn unknown_codec_is_none() {
        assert!(packetizer_for(CodecId::Av1).is_none());
    }
}
