//! `aac_adtstoasc`: strip ADTS framing from raw AAC and synthesise the
//! `AudioSpecificConfig` extradata a bare-stream consumer (MP4, an `esds`
//! box) needs instead. MPEG-TS carries AAC as ADTS and MP4 wants an
//! `esds`/`AudioSpecificConfig` up front, so a remux between them needs it.
//!
//! # Specification
//!
//! ISO/IEC 13818-7 Annex B (`provenance/sources.toml`'s `iso-13818-7`) for
//! `adts_fixed_header()`/`adts_variable_header()`; ISO/IEC 14496-3 §1.6.2.1
//! for `AudioSpecificConfig()`. Every field was cross-checked against a
//! real encode.
//!
//! # What is measured, not assumed
//!
//! `ffmpeg -c:a aac -f adts` on a sine wave, then `-bsf:a aac_adtstoasc`
//! compared against an `esds` box from an equivalent MP4 remux (parsed as
//! nested MPEG-4 descriptors: tag, then a multi-byte length varint):
//!
//! * ADTS header: `fixed(7 bytes, protection_absent=1)`. `profile=1`
//!   (`audioObjectType = profile + 1 = 2`, AAC-LC), `sampling_frequency_index=4`
//!   (44100 Hz), `channel_configuration=1` (mono).
//! * `AudioSpecificConfig` really written: `12 08` — `0b00010_0100_0001_000`,
//!   i.e. `audioObjectType(5) | samplingFrequencyIndex(4) | channelConfiguration(4)`
//!   then three zero bits (`frameLengthFlag`, `dependsOnCoreCoder`,
//!   `extensionFlag`) — 16 bits, exactly reproducing the measured value from
//!   the ADTS fields above with no extra encoding step.
//! * Packet payload: ADTS's own 7-byte header stripped from every packet
//!   (`265` bytes in `-> 258`), and the new extradata attached to the
//!   **first** packet only (`PacketSideData::NewExtradata`) — later packets
//!   with an unchanged ADTS header carry no side data.
//!
//! # What this does not cover
//!
//! `number_of_raw_data_blocks_in_frame != 0` — more than one AAC frame
//! behind one ADTS header, with a `crc_check` per block when
//! `protection_absent == 0`. Every encoder measured here writes `0` blocks;
//! this filter refuses (`Error::Unsupported`) rather than guess at the
//! multi-block layout with nothing to check it against. Likewise
//! `sampling_frequency_index == 15` (an escape value ADTS's fixed header has
//! no room to carry an explicit frequency for) is refused.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData};
use vaco_pool::Buffer;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "aac_adtstoasc",
    long_name: "Convert MPEG-2/4 AAC ADTS to an MPEG-4 Audio Specific Configuration bitstream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Aac) => Ok(Box::new(MappedFilter::new(AdtsToAsc {
            stored: None,
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("aac_adtstoasc: aac only")),
    }
}

struct AdtsToAsc {
    /// The `AudioSpecificConfig` last attached as `NewExtradata`, so an
    /// unchanged ADTS header does not re-attach it on every packet.
    stored: Option<[u8; 2]>,
    budget: Budget,
}

/// One ADTS frame's fixed-header fields this filter needs.
struct AdtsHeader {
    /// Total header length: 7 bytes (`protection_absent`) or 9
    /// (a `crc_check` follows, single block only).
    header_len: usize,
    audio_object_type: u8,
    sampling_frequency_index: u8,
    channel_configuration: u8,
}

/// Parse one ADTS frame's fixed and variable header, per ISO/IEC 13818-7
/// Annex B. `None` for anything shorter than a header, a bad syncword, or a
/// shape this filter does not cover (see the module docs).
fn parse_adts_header(data: &[u8]) -> Option<AdtsHeader> {
    let &[b0, b1, b2, b3, _b4, _b5, b6, ..] = data else {
        return None;
    };
    if b0 != 0xFF || (b1 & 0xF0) != 0xF0 {
        return None; // syncword: 12 ones.
    }
    let protection_absent = b1 & 0x01;
    let profile = (b2 >> 6) & 0x03;
    let sampling_frequency_index = (b2 >> 2) & 0x0F;
    let channel_configuration = ((b2 & 0x01) << 2) | (b3 >> 6);
    let number_of_raw_data_blocks = b6 & 0x03;

    if sampling_frequency_index == 15 || number_of_raw_data_blocks != 0 {
        return None; // not covered — see the module docs.
    }
    let header_len = if protection_absent == 1 { 7 } else { 9 };
    if data.len() < header_len {
        return None;
    }
    Some(AdtsHeader {
        header_len,
        audio_object_type: profile + 1,
        sampling_frequency_index,
        channel_configuration,
    })
}

/// `AudioSpecificConfig()`, ISO/IEC 14496-3 §1.6.2.1, `GASpecificConfig`'s
/// three flag bits all zero (measured — see the module docs).
fn audio_specific_config(h: &AdtsHeader) -> [u8; 2] {
    let value: u16 = (u16::from(h.audio_object_type) << 11)
        | (u16::from(h.sampling_frequency_index) << 7)
        | (u16::from(h.channel_configuration) << 3);
    value.to_be_bytes()
}

impl PacketMap for AdtsToAsc {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let payload = p.payload();
        let Some(header) = parse_adts_header(payload) else {
            return Err(Error::Unsupported(
                "aac_adtstoasc: not a single-block ADTS frame this filter covers",
            ));
        };
        let raw_aac = payload.get(header.header_len..).unwrap_or(&[]);

        let mut np = Packet::from_slice(&mut self.budget, raw_aac)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;

        let asc = audio_specific_config(&header);
        if self.stored != Some(asc) {
            self.stored = Some(asc);
            let buf = Buffer::from_slice(&mut self.budget, &asc)?;
            np.side_data.push(PacketSideData::NewExtradata(buf));
        }
        out.push_back(np);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn aac_params() -> CodecParameters {
        CodecParameters::audio().with_codec(CodecId::Aac)
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    fn side_data_extradata(p: &Packet) -> Option<&[u8]> {
        p.side_data.iter().find_map(|sd| match sd {
            PacketSideData::NewExtradata(b) => Some(b.as_slice()),
            _ => None,
        })
    }

    /// The exact measured example: profile=1 (AAC-LC), sfi=4 (44100 Hz),
    /// chan=1 (mono), `protection_absent=1` (7-byte header). Real bytes off a
    /// `libavcodec` `aac` encoder's ADTS output.
    fn measured_adts_header() -> [u8; 7] {
        [0xFF, 0xF1, 0x50, 0x40, 0x21, 0x3F, 0xFC]
    }

    #[test]
    fn header_is_stripped_and_the_measured_asc_is_attached() {
        let mut frame = measured_adts_header().to_vec();
        frame.extend_from_slice(&[0xAA; 10]);
        let mut f = (DESC.build)(&aac_params()).unwrap();
        f.send_packet(Some(&pkt(&frame))).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(out.payload(), &[0xAA; 10]);
        assert_eq!(side_data_extradata(&out), Some(&[0x12, 0x08][..]));
    }

    #[test]
    fn an_unchanging_header_emits_extradata_once() {
        let mut frame = measured_adts_header().to_vec();
        frame.extend_from_slice(&[0xAA; 4]);
        let mut f = (DESC.build)(&aac_params()).unwrap();
        f.send_packet(Some(&pkt(&frame))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_some());
        f.send_packet(Some(&pkt(&frame))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_none());
    }

    #[test]
    fn a_changed_configuration_reattaches_extradata() {
        let mut mono = measured_adts_header().to_vec();
        mono.extend_from_slice(&[0xAA; 4]);
        // Same header but channel_configuration changed from 1 to 2
        // (stereo): byte3's top two bits carry the low two channel bits.
        let mut stereo_header = measured_adts_header();
        stereo_header[3] = (stereo_header[3] & 0x3F) | 0x80; // chan low bits -> 10
        let mut stereo = stereo_header.to_vec();
        stereo.extend_from_slice(&[0xAA; 4]);

        let mut f = (DESC.build)(&aac_params()).unwrap();
        f.send_packet(Some(&pkt(&mono))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_some());
        f.send_packet(Some(&pkt(&stereo))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_some());
    }

    #[test]
    fn a_bad_syncword_is_refused() {
        let mut frame = measured_adts_header();
        frame[0] = 0x00;
        let mut f = (DESC.build)(&aac_params()).unwrap();
        assert!(f.send_packet(Some(&pkt(&frame))).is_err());
    }

    #[test]
    fn multiple_raw_data_blocks_are_refused_rather_than_guessed() {
        let mut frame = measured_adts_header();
        frame[6] |= 0x01; // number_of_raw_data_blocks_in_frame = 1
        let mut f = (DESC.build)(&aac_params()).unwrap();
        assert!(f.send_packet(Some(&pkt(&frame))).is_err());
    }

    #[test]
    fn a_non_aac_codec_is_refused_at_construction() {
        let params = CodecParameters::audio().with_codec(CodecId::Opus);
        assert!((DESC.build)(&params).is_err());
    }

    /// Falsifies the "`protection_absent` doesn't change the header length"
    /// misreading: with a CRC present the header is 9 bytes, not 7, and
    /// stripping only 7 would leave 2 CRC bytes glued onto the raw AAC.
    #[test]
    fn falsified_ignoring_protection_absent_would_mis_size_the_header() {
        let mut with_crc = measured_adts_header();
        with_crc[1] &= 0xFE; // protection_absent = 0
        let mut frame = with_crc.to_vec();
        frame.extend_from_slice(&[0x00, 0x00]); // the crc_check itself
        frame.extend_from_slice(&[0xAA; 4]);
        let mut f = (DESC.build)(&aac_params()).unwrap();
        f.send_packet(Some(&pkt(&frame))).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(
            out.payload(),
            &[0xAA; 4],
            "the 2 CRC bytes must also be stripped"
        );
    }
}
