//! [`H264Decoder`] — the [`Decoder`] this crate registers.
//!
//! Mirrors `vaco-codec-aac::AacDecoder`'s precedent exactly: resolve as much
//! of the packet as this dispatch's scope covers, then return
//! [`vaco_core::Error::Unsupported`] naming precisely what is missing,
//! rather than either refusing the packet outright or fabricating a frame.
//! What this dispatch covers is the entropy layer only — [`crate::cavlc`]
//! and [`crate::cabac_residual`]'s residual-block decode — not the
//! macroblock layer (#419+) that would drive them across a real slice and
//! turn their output into pixels. So today, [`H264Decoder::send_packet`]
//! locates the slice header's `pic_parameter_set_id` (clause 7.3.3, the
//! three `ue(v)` fields preceding it), resolves the active PPS/SPS pair
//! (borrowed from an internal `vaco_parse_h264::H264Parser`, already built
//! and reference-tested — parameter-set bookkeeping is not re-implemented
//! here), reads `entropy_coding_mode_flag` off the PPS, and stops there.
//!
//! Parameter sets reach this decoder only through
//! [`Decoder::set_extradata`] (avcC / out-of-band, the common MP4/Matroska
//! case) today. In-band SPS/PPS NAL units interleaved with slice data in an
//! Annex-B byte stream are not collected by this decoder — doing that
//! without also assembling access units the way `H264Parser::push_access_unit`
//! does would duplicate that logic rather than reuse it, and this crate's
//! scope stops well before an access unit is ever fully decoded regardless.

use vaco_bitstream::BitReader;
use vaco_codec_core::Decoder;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_h264::{H264NalHeader, H264Parser};

/// The H.264 decoder. See the module doc for exactly what is and is not
/// implemented today.
#[derive(Debug)]
pub struct H264Decoder {
    limits: Limits,
    parser: H264Parser,
}

impl H264Decoder {
    /// Build a decoder bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: H264Parser::new(limits.clone()),
            limits,
        }
    }
}

impl Decoder for H264Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            return Err(Error::Eof);
        };
        let payload = packet.payload();
        let nal = H264NalHeader::parse(payload).ok_or(Error::InvalidData("empty NAL unit"))?;
        if !nal.nal_unit_type.has_slice_header() {
            return Err(Error::Unsupported(
                "vaco-codec-h264: packet does not open on a slice NAL unit \
                 (parameter-set/SEI-only access units are not decoded — \
                 vaco-parse-h264 handles those for container reporting)",
            ));
        }

        // clause 7.3.3, up to and including pic_parameter_set_id: the three
        // ue(v) fields every slice_header() opens with, regardless of slice
        // type or profile. first_mb_in_slice's true upper bound needs the
        // active SPS's own frame size, which is exactly what has not been
        // resolved yet at this point — bounding it by u32::MAX instead
        // (BoundedGolomb still enforces D6's allocation ceiling through
        // `budget`, just not this specific semantic bound) is correct
        // because nothing is allocated from this value here.
        let mut reader = BitReader::new(payload);
        reader.skip(8); // the NAL header byte
        let mut budget = Budget::new(self.limits.clone());
        let mut g = BoundedGolomb::new(&mut reader, &mut budget);
        let _first_mb_in_slice = g.ue_v(u32::MAX)?;
        let _slice_type = g.ue_v(9)?;
        let pps_id = g.ue_v(255)? as u8;

        let (pps, _sps) = self
            .parser
            .parameter_sets()
            .sps_for_pps(pps_id)
            .ok_or(Error::Unsupported("vaco-codec-h264: referenced PPS/SPS not seen yet"))?;
        let mode = if pps.entropy_coding_mode { "CABAC" } else { "CAVLC" };

        Err(Error::Unsupported(match mode {
            "CABAC" => {
                "vaco-codec-h264: slice header located, entropy_coding_mode_flag \
                 resolved to CABAC (crate::cabac_residual implements the \
                 residual-block level of this), but the macroblock layer that \
                 drives it across a slice — mb_type, prediction, transform \
                 reconstruction (#419 onward) — is not implemented; see \
                 docs/codec/vaco-codec-h264.md"
            }
            _ => {
                "vaco-codec-h264: slice header located, entropy_coding_mode_flag \
                 resolved to CAVLC (crate::cavlc implements the residual-block \
                 level of this), but the macroblock layer that drives it across \
                 a slice — mb_type, prediction, transform reconstruction (#419 \
                 onward) — is not implemented; see docs/codec/vaco-codec-h264.md"
            }
        }))
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        Err(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.parser.flush();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }
}
