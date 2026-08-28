//! [`AacDecoder`] — the [`Decoder`] implementation this crate registers.
//!
//! # What this decoder can and cannot do today
//!
//! It resolves configuration completely: `set_extradata` for an
//! out-of-band `AudioSpecificConfig` (MP4/LATM), or a leading ADTS header
//! read straight off the payload for a raw AAC stream, object-type gating,
//! and channel-configuration resolution including reading a leading program
//! config element when needed (T3-03a / #443, this crate's actual scope).
//!
//! **It does not decode spectral data.** Window sequences and shapes,
//! scalefactor bands, section data and the Huffman codebooks (T3-03b /
//! #444), inverse quantisation, TNS, joint stereo and the IMDCT (T3-03c /
//! #445) are unimplemented. [`AacDecoder::send_packet`] accepts a packet,
//! fully resolves its configuration, and then returns
//! [`Error::Unsupported`] rather than either refusing to accept the packet
//! at all or fabricating output it cannot yet produce correctly — the same
//! choice this workspace made for MPEG-2.5 Layer III, for the same reason.

use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::Limits;
use vaco_packet::Packet;
use vaco_parse_aac::{AdtsHeader, AudioSpecificConfig};

use crate::config::DecoderConfig;

/// The AAC-LC decoder. See the module doc for exactly what is and is not
/// implemented yet.
#[derive(Debug)]
pub struct AacDecoder {
    #[expect(dead_code, reason = "sized allocations are #444/#445's concern; carried now so the constructor's shape does not change later")]
    limits: Limits,
    /// Set by [`Decoder::set_extradata`], when the container offered one.
    extradata_config: Option<DecoderConfig>,
    /// The configuration currently in force — from `extradata_config` if
    /// present, otherwise (re-)derived per packet from a leading ADTS
    /// header.
    config: Option<DecoderConfig>,
}

impl AacDecoder {
    /// Build a decoder bounded by `limits`. `limits` is not consulted yet —
    /// #444/#445's spectral-array and PCM-frame allocations are what would
    /// need to charge against it — but every other `Decoder` in this
    /// workspace takes one at construction, and taking it now means the
    /// constructor's signature does not change out from under
    /// `vaco-registry`'s generated `make: fn(Limits) -> Box<dyn Decoder>`
    /// the day spectral decode lands.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            extradata_config: None,
            config: None,
        }
    }

    /// Resolve this packet's configuration: reuse `extradata_config` if one
    /// was set, otherwise parse a leading ADTS header off the payload
    /// itself. If the resolved configuration is still waiting on a program
    /// config element (`channelConfiguration == 0`), try to clear that from
    /// the payload's own leading bits too.
    fn resolve_packet_config(&mut self, payload: &[u8]) -> Result<DecoderConfig> {
        let mut cfg = if let Some(cfg) = &self.extradata_config {
            cfg.clone()
        } else {
            let header = AdtsHeader::parse(payload)?;
            DecoderConfig::from_adts_header(&header)?
        };
        if cfg.is_pending() {
            // The raw_data_block starts right after the ADTS header for the
            // no-extradata path; for the extradata path the whole payload
            // already *is* the raw_data_block (MP4/LATM carry no per-frame
            // ADTS header at all).
            let body = if self.extradata_config.is_some() {
                payload
            } else {
                let header = AdtsHeader::parse(payload)?;
                let start = header.header_len();
                payload.get(start..).unwrap_or(&[])
            };
            let mut r = vaco_bitstream::BitReader::new(body);
            let _ = cfg.try_resolve_pending(&mut r)?;
        }
        Ok(cfg)
    }
}

impl Decoder for AacDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            // Draining at EOF: nothing is ever buffered (every real call
            // errors before reaching a state that would need to be), so
            // there is nothing to flush out here.
            return Err(Error::Eof);
        };
        let cfg = self.resolve_packet_config(packet.payload())?;
        self.config = Some(cfg);
        Err(Error::Unsupported(
            "vaco-codec-aac: configuration resolved, but spectral decode (window \
             sequences, scalefactor bands, Huffman decode, TNS, joint stereo, \
             IMDCT) is not implemented — see docs/codec/vaco-codec-aac.md",
        ))
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        Err(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.config = None;
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let asc = AudioSpecificConfig::parse(extradata)?;
        self.extradata_config = Some(DecoderConfig::from_audio_specific_config(&asc)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code")]
    use super::AacDecoder;
    use vaco_codec_core::Decoder;
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;

    fn adts_stereo_frame() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 2); // stereo
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7); // header-only frame
        w.put(11, 0x7ff);
        w.put(2, 0);
        w.finish()
    }

    #[test]
    fn a_resolvable_packet_is_rejected_only_at_the_not_implemented_boundary() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_stereo_frame();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        let err = dec.send_packet(Some(&packet)).unwrap_err();
        // The point is *which* error: configuration must have resolved
        // successfully (a config-layer bug would surface as InvalidData or
        // Unsupported for the wrong reason, not this one), and decode itself
        // is the only thing missing.
        assert!(format!("{err}").contains("spectral decode"));
    }

    #[test]
    fn draining_with_nothing_sent_reports_eof() {
        let mut dec = AacDecoder::new(Limits::permissive());
        assert!(dec.send_packet(None).is_err());
    }
}
