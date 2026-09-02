//! `hevc_mp4toannexb`: length-prefixed HEVC to Annex B, with parameter sets.
//!
//! Same shape as [`crate::h264_mp4toannexb`]; see that module for the general
//! approach. What was measured separately rather than assumed symmetric:
//!
//! * The splice trigger is an **IRAP** access unit
//!   ([`vaco_parse_hevc::nal::NalUnitType::is_irap`], `nal_unit_type` 16..=23
//!   per ITU-T H.265 §3.73), not specifically an IDR — HEVC's random-access
//!   points include `CRA`/`BLA` as well as `IDR`, and there is no HEVC reason
//!   to special-case just the two IDR types the way H.264 only has one
//!   concept at all.
//! * The record carries **VPS, SPS and PPS**, not two lists — spliced in
//!   that order, matching `HevcDecoderConfigurationRecord::vps`/`sps`/`pps`'s
//!   own iteration order and the 3-unit VPS/SPS/PPS sequence measured ahead
//!   of an IRAP in a real `libx265` stream.
//! * **Every unit gets a 4-byte Annex B start code, with no exception** —
//!   checked on the exact experiment that found H.264's one cosmetic
//!   exception (see [`crate::h264_mp4toannexb`]'s docs) and confirmed HEVC
//!   does not share it. Implemented uniformly, matching
//!   `vaco_format_nalu::convert`'s own convention.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::Result;
use vaco_format_nalu::{
    Framing, HeaderKind, LengthSize, NalHeader, convert::length_prefixed_to_annexb, units,
};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::HevcDecoderConfigurationRecord;
use vaco_parse_hevc::nal::NalUnitType;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "hevc_mp4toannexb",
    long_name: "Convert an HEVC bitstream from length prefixed to Annex B",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    if params.codec_id != Some(CodecId::Hevc) {
        return Err(vaco_core::Error::Unsupported(
            "hevc_mp4toannexb: this filter only handles hevc",
        ));
    }
    let length_size = params
        .video
        .as_ref()
        .and_then(|v| v.nal_length_size)
        .and_then(LengthSize::new);

    let Some(length_size) = length_size else {
        return Ok(Box::new(MappedFilter::new(HevcMp4ToAnnexb {
            length_size: None,
            param_sets: Vec::new(),
            convert_budget: Budget::new(Limits::permissive()),
            out_budget: Budget::new(Limits::permissive()),
        })));
    };

    let param_sets = params.extradata.as_deref().map_or_else(Vec::new, |extra| {
        if extra.is_empty() {
            return Vec::new();
        }
        let mut budget = Budget::new(Limits::permissive());
        HevcDecoderConfigurationRecord::parse(extra, &mut budget).map_or_else(
            |_| Vec::new(),
            |record| {
                let mut buf = Vec::new();
                for unit in record.vps().chain(record.sps()).chain(record.pps()) {
                    buf.extend_from_slice(&[0, 0, 0, 1]);
                    buf.extend_from_slice(unit);
                }
                buf
            },
        )
    });

    Ok(Box::new(MappedFilter::new(HevcMp4ToAnnexb {
        length_size: Some(length_size),
        param_sets,
        convert_budget: Budget::new(Limits::permissive()),
        out_budget: Budget::new(Limits::permissive()),
    })))
}

struct HevcMp4ToAnnexb {
    length_size: Option<LengthSize>,
    param_sets: Vec<u8>,
    convert_budget: Budget,
    out_budget: Budget,
}

impl PacketMap for HevcMp4ToAnnexb {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let Some(length_size) = self.length_size else {
            out.push_back(p.clone());
            return Ok(());
        };

        let mut annexb = Vec::new();
        length_prefixed_to_annexb(
            p.payload(),
            length_size,
            &mut annexb,
            &mut self.convert_budget,
        )?;

        let final_bytes = if self.param_sets.is_empty() {
            annexb
        } else {
            splice_before_first_irap(&annexb, &self.param_sets)
        };

        let mut np = Packet::from_slice(&mut self.out_budget, &final_bytes)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;
        np.side_data.clone_from(&p.side_data);
        out.push_back(np);
        Ok(())
    }
}

/// Insert `param_sets` immediately before the first IRAP unit in `annexb`.
fn splice_before_first_irap(annexb: &[u8], param_sets: &[u8]) -> Vec<u8> {
    for nal in units(annexb, Framing::AnnexB) {
        let Some(header) = NalHeader::parse(HeaderKind::H265, nal.data) else {
            continue;
        };
        if !NalUnitType::from_u8(header.nal_unit_type).is_irap() {
            continue;
        }
        let sc_start = nal.offset.saturating_sub(usize::from(nal.start_code_len));
        let mut out = Vec::new();
        out.extend_from_slice(annexb.get(..sc_start).unwrap_or(annexb));
        out.extend_from_slice(param_sets);
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal.data);
        out.extend_from_slice(annexb.get(nal.end()..).unwrap_or(&[]));
        return out;
    }
    annexb.to_vec()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn lp_pkt(nals: &[&[u8]]) -> Packet {
        let mut buf = Vec::new();
        for n in nals {
            buf.extend_from_slice(&(n.len() as u32).to_be_bytes());
            buf.extend_from_slice(n);
        }
        Packet::from_slice(&mut budget(), &buf).unwrap()
    }

    /// A minimal, well-formed `hvcC` carrying one VPS, one SPS and one PPS,
    /// each in its own `NalArray`.
    fn hvcc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
        let mut r = vec![1u8]; // configurationVersion
        r.extend_from_slice(&[0u8; 21]); // profile/level/compat/misc fields, all zeroed
        r.push(3); // numOfArrays
        for (nal_type, unit) in [(32u8, vps), (33, sps), (34, pps)] {
            r.push(0x80 | (nal_type & 0x3F)); // array_completeness=1, NAL_unit_type
            r.extend_from_slice(&1u16.to_be_bytes()); // numNalus
            r.extend_from_slice(&(unit.len() as u16).to_be_bytes());
            r.extend_from_slice(unit);
        }
        r
    }

    fn hevc_params(extra: Vec<u8>) -> CodecParameters {
        let mut p = CodecParameters::video().with_codec(CodecId::Hevc);
        p.extradata = Some(extra);
        p.video = Some(VideoParameters {
            nal_length_size: Some(4),
            ..VideoParameters::default()
        });
        p
    }

    #[test]
    fn an_irap_gets_vps_sps_pps_spliced_before_it() {
        let vps = [0x40, 0x01, 0xAA];
        let sps = [0x42, 0x01, 0xBB];
        let pps = [0x44, 0x01, 0xCC];
        // IDR_W_RADL = 19: header byte0 = 0_010011_0 = 0x26, byte1 = tid_plus1=1.
        let idr = [0x26, 0x01, 0x88];
        let params = hevc_params(hvcc(&vps, &sps, &pps));
        let mut f = (DESC.build)(&params).unwrap();

        f.send_packet(Some(&lp_pkt(&[&idr]))).unwrap();
        let out = f.receive_packet().unwrap();
        let mut expected = Vec::new();
        for u in [&vps[..], &sps[..], &pps[..], &idr[..]] {
            expected.extend_from_slice(&[0, 0, 0, 1]);
            expected.extend_from_slice(u);
        }
        assert_eq!(out.payload(), expected.as_slice());
    }

    #[test]
    fn a_trail_r_frame_gets_no_splice() {
        let vps = [0x40, 0x01, 0xAA];
        let sps = [0x42, 0x01, 0xBB];
        let pps = [0x44, 0x01, 0xCC];
        // TRAIL_R = 1: byte0 = 0_000001_0 = 0x02.
        let trail = [0x02, 0x01, 0x9A];
        let params = hevc_params(hvcc(&vps, &sps, &pps));
        let mut f = (DESC.build)(&params).unwrap();

        f.send_packet(Some(&lp_pkt(&[&trail]))).unwrap();
        let out = f.receive_packet().unwrap();
        let mut expected = vec![0, 0, 0, 1];
        expected.extend_from_slice(&trail);
        assert_eq!(out.payload(), expected.as_slice());
    }

    #[test]
    fn an_already_annexb_stream_passes_through_unchanged() {
        let annexb = [0, 0, 0, 1, 0x40, 0x01, 0, 0, 0, 1, 0x26, 0x01];
        let params = CodecParameters::video().with_codec(CodecId::Hevc);
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(&Packet::from_slice(&mut budget(), &annexb).unwrap()))
            .unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &annexb);
    }

    #[test]
    fn an_unsupported_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::H264);
        assert!((DESC.build)(&params).is_err());
    }
}
