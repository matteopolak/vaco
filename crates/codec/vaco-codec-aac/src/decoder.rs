//! [`AacDecoder`] — the [`Decoder`] implementation this crate registers.
//!
//! # What this decoder can and cannot do today
//!
//! It resolves configuration completely (T3-03a / #443) and fully parses
//! `raw_data_block()`'s syntax (T3-03b / #444): window sequences and
//! shapes, section data, scalefactor/intensity/noise DPCM decode, pulse
//! data, TNS syntax (read, not applied), and the spectral Huffman codebooks
//! — every bit the frame declares is consumed, and the reader lands exactly
//! where the next frame begins. What it does not do is turn that parsed
//! syntax into PCM: inverse quantisation, TNS application, joint stereo
//! (M/S and intensity) and the IMDCT/overlap-add (T3-03c / #445) are
//! unimplemented. [`AacDecoder::send_packet`] accepts a packet, fully
//! resolves and parses it, and only then returns [`Error::Unsupported`] —
//! never fabricating PCM it cannot yet produce correctly, the same choice
//! this workspace made for MPEG-2.5 Layer III.

use vaco_bitstream::BitReader;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::Limits;
use vaco_packet::Packet;
use vaco_parse_aac::{AdtsHeader, AudioSpecificConfig};

use crate::config::DecoderConfig;
use crate::raw_data_block;

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

    /// Resolve this packet's configuration and locate its `raw_data_block()`
    /// body: reuse `extradata_config` if one was set (the whole payload is
    /// then the `raw_data_block` — MP4/LATM carry no per-frame ADTS header),
    /// otherwise parse a leading `AdtsHeader` and take the body from just
    /// past it. If the resolved configuration is still waiting on a program
    /// config element (`channelConfiguration == 0`), try to clear that from
    /// the body's own leading bits too.
    fn resolve_packet<'a>(&mut self, payload: &'a [u8]) -> Result<(DecoderConfig, &'a [u8])> {
        let (mut cfg, body) = if let Some(cfg) = &self.extradata_config {
            (cfg.clone(), payload)
        } else {
            let header = AdtsHeader::parse(payload)?;
            let cfg = DecoderConfig::from_adts_header(&header)?;
            let body = payload.get(header.header_len()..).unwrap_or(&[]);
            (cfg, body)
        };
        if cfg.is_pending() {
            let mut r = BitReader::new(body);
            let _ = cfg.try_resolve_pending(&mut r)?;
        }
        Ok((cfg, body))
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
        let (cfg, body) = self.resolve_packet(packet.payload())?;
        if cfg.is_pending() {
            return Err(Error::Unsupported(
                "vaco-codec-aac: channelConfiguration == 0 and no leading program \
                 config element was found; cannot determine channel layout",
            ));
        }
        let sfi = vaco_parse_aac::tables::index_for_frequency(cfg.sample_rate);
        let mut r = BitReader::new(body);
        let elements = raw_data_block::read(&mut r, sfi)?;
        self.config = Some(cfg);
        Err(Error::Unsupported(
            match elements.len() {
                0 => "vaco-codec-aac: raw_data_block parsed with no audio elements",
                _ => "vaco-codec-aac: raw_data_block fully parsed (window sequences, \
                      section data, scalefactor decode, pulse data, TNS syntax, \
                      spectral Huffman decode), but reconstruction — inverse \
                      quantisation, TNS application, joint stereo, IMDCT/overlap-add \
                      — is not implemented — see docs/codec/vaco-codec-aac.md",
            },
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
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::AacDecoder;
    use vaco_codec_core::Decoder;
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;

    /// A real ADTS header (mono, so a single `SCE` is the whole
    /// `raw_data_block`) wrapping a minimal-but-complete `SCE` (`max_sfb=1`,
    /// one `ZERO_HCB` band) followed by `ID_END` and `byte_alignment()`.
    fn adts_frame_with_minimal_raw_data_block() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 0); // ID_SCE
        body.put(4, 0); // element_instance_tag
        body.put(8, 100); // global_gain
        body.put(1, 0); // ics_reserved_bit
        body.put(2, 0); // ONLY_LONG
        body.put(1, 0); // sine window
        body.put(6, 1); // max_sfb = 1
        body.put(1, 0); // predictor_data_present
        body.put(4, 0); // sect_cb = ZERO_HCB
        body.put(5, 1); // sect_len = 1
        body.put(1, 0); // pulse_data_present
        body.put(1, 0); // tns_data_present
        body.put(1, 0); // gain_control_data_present
        body.put(3, 7); // ID_END
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 1); // mono
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    #[test]
    fn a_fully_parsed_frame_is_rejected_only_at_the_reconstruction_boundary() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        let err = dec.send_packet(Some(&packet)).unwrap_err();
        // The point is *which* error: configuration and the full
        // raw_data_block syntax (ics_info, section_data, scale_factor_data,
        // pulse/TNS presence, spectral_data) must all have parsed
        // successfully — a bug anywhere in that chain would surface as a
        // different error — and only reconstruction (PCM synthesis) is
        // missing.
        assert!(format!("{err}").contains("reconstruction"), "{err}");
    }

    #[test]
    fn draining_with_nothing_sent_reports_eof() {
        let mut dec = AacDecoder::new(Limits::permissive());
        assert!(dec.send_packet(None).is_err());
    }
}
