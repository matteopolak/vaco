//! `h264_mp4toannexb`: length-prefixed H.264 to Annex B, with parameter sets.
//!
//! # What is measured
//!
//! Framing conversion alone (`vaco_format_nalu::convert::length_prefixed_to_annexb`)
//! is not what the reference does. Checked against `ffmpeg 8.1` on an
//! `avcC`-framed two-keyframe MP4 (`-g 25`, 2 s at 25 fps): every access unit
//! whose first VCL NAL is an IDR slice (`nal_unit_type == 5`) gets the
//! `avcC`'s SPS and PPS spliced in, **immediately before that IDR unit** and
//! after any leading non-VCL unit already in the access unit (a leading SEI
//! was left in place ahead of the splice in both keyframes tested) — 2
//! insertions for 2 keyframes, 0 for the 23 inter frames between them. A
//! stream with no `avcC` extradata, or whose `nal_length_size` already reads
//! as Annex B (`0`/absent), is a pass-through: run directly on an
//! already-Annex-B stream, output was byte-identical to input.
//!
//! **The one place this filter departs from `vaco_format_nalu::convert`'s
//! "four is what every producer writes" default**: the reference writes the
//! NAL unit immediately following a parameter-set insertion with a 3-byte
//! Annex B start code where every other unit — including that same slot
//! when nothing was inserted before it — gets 4. Measured on both keyframes
//! for H.264, and confirmed *absent* on the HEVC sibling filter under the
//! identical experiment (every unit there gets 4 regardless of position),
//! so it is not a general framing rule — [`splice_before_first_idr`] special-
//! cases only its own insertion point, and every other NAL this filter
//! converts (via `length_prefixed_to_annexb`) keeps the shared 4-byte
//! convention. Originally left unreproduced as "not worth a knob" before a
//! real byte-exact `-c copy` remux comparison made it a genuine, measured
//! divergence (`planning/CONFORMANCE-FINDINGS.md` finding 57, cases 34/36/39)
//! rather than a cosmetic one worth discounting.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::Result;
use vaco_format_nalu::{Framing, HeaderKind, LengthSize, NalHeader, convert::length_prefixed_to_annexb, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_h264::AvcDecoderConfigurationRecord;

/// H.264's IDR slice `nal_unit_type` (ITU-T H.264 Table 7-1).
const IDR_SLICE: u8 = 5;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "h264_mp4toannexb",
    long_name: "Convert an H.264 bitstream from length prefixed to Annex B",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    if params.codec_id != Some(CodecId::H264) {
        return Err(vaco_core::Error::Unsupported(
            "h264_mp4toannexb: this filter only handles h264",
        ));
    }
    let length_size = params
        .video
        .as_ref()
        .and_then(|v| v.nal_length_size)
        .and_then(LengthSize::new);

    // No length prefix declared: already Annex B (or the question does not
    // apply), so this is a pass-through — measured directly.
    let Some(length_size) = length_size else {
        return Ok(Box::new(MappedFilter::new(H264Mp4ToAnnexb {
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
        AvcDecoderConfigurationRecord::parse(extra, &mut budget).map_or_else(
            // A malformed record still gets the framing conversion; it just
            // never has parameter sets to splice. Graceful degradation, not
            // an error — a caller asked to reframe, not to validate.
            |_| Vec::new(),
            |record| {
                let mut buf = Vec::new();
                for s in record.sps.iter().chain(record.pps.iter()) {
                    buf.extend_from_slice(&[0, 0, 0, 1]);
                    buf.extend_from_slice(s);
                }
                buf
            },
        )
    });

    Ok(Box::new(MappedFilter::new(H264Mp4ToAnnexb {
        length_size: Some(length_size),
        param_sets,
        convert_budget: Budget::new(Limits::permissive()),
        out_budget: Budget::new(Limits::permissive()),
    })))
}

struct H264Mp4ToAnnexb {
    /// `None` means "pass through unchanged" (already Annex B).
    length_size: Option<LengthSize>,
    /// Annex-B-framed SPS then PPS units, built once from `avcC`. Empty means
    /// "nothing to splice" (no usable extradata).
    param_sets: Vec<u8>,
    convert_budget: Budget,
    out_budget: Budget,
}

impl PacketMap for H264Mp4ToAnnexb {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let Some(length_size) = self.length_size else {
            out.push_back(p.clone());
            return Ok(());
        };

        let mut annexb = Vec::new();
        length_prefixed_to_annexb(p.payload(), length_size, &mut annexb, &mut self.convert_budget)?;

        let final_bytes = if self.param_sets.is_empty() {
            annexb
        } else {
            splice_before_first_idr(&annexb, &self.param_sets)
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

/// Insert `param_sets` immediately before the first IDR slice unit in
/// `annexb`, leaving every other byte untouched. No IDR found means no splice
/// — an inter-frame access unit is returned as-is.
fn splice_before_first_idr(annexb: &[u8], param_sets: &[u8]) -> Vec<u8> {
    for nal in units(annexb, Framing::AnnexB) {
        let Some(header) = NalHeader::parse(HeaderKind::H264, nal.data) else {
            continue;
        };
        if header.nal_unit_type != IDR_SLICE {
            continue;
        }
        let sc_start = nal.offset.saturating_sub(usize::from(nal.start_code_len));
        let mut out = Vec::new();
        out.extend_from_slice(annexb.get(..sc_start).unwrap_or(annexb));
        out.extend_from_slice(param_sets);
        // Three bytes, not four, here specifically — see this module's own
        // doc comment ("Not reproduced", now reproduced): measured against
        // `ffmpeg 9.0.1` on a real `-c copy` MP4->MPEG-TS remux, the one NAL
        // unit immediately following a parameter-set splice gets a 3-byte
        // Annex B start code where every other unit (including this same
        // IDR slot when nothing was spliced ahead of it, and every unit
        // `length_prefixed_to_annexb` converts on its own) gets 4. Confirmed
        // absent on the HEVC sibling filter under the identical experiment,
        // so this is not a general Annex-B convention this crate should
        // apply anywhere else — narrowly, only right after this splice.
        out.extend_from_slice(&[0, 0, 1]);
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

    fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
        // Minimal, well-formed AvcDecoderConfigurationRecord: version 1,
        // profile/compat/level from the SPS's own bytes, length_size_minus_one=3,
        // one SPS, one PPS.
        let mut r = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
        r.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        r.extend_from_slice(sps);
        r.push(1);
        r.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        r.extend_from_slice(pps);
        r
    }

    fn h264_params(extra: Vec<u8>) -> CodecParameters {
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        p.extradata = Some(extra);
        p.video = Some(VideoParameters {
            nal_length_size: Some(4),
            ..VideoParameters::default()
        });
        p
    }

    #[test]
    fn a_keyframe_gets_parameter_sets_spliced_before_the_idr() {
        let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
        let pps = [0x68, 0xEB];
        let idr = [0x65, 0x88, 0x84];
        let params = h264_params(avcc(&sps, &pps));
        let mut f = (DESC.build)(&params).unwrap();

        f.send_packet(Some(&lp_pkt(&[&idr]))).unwrap();
        let out = f.receive_packet().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&pps);
        // 3 bytes, not 4: the IDR immediately follows the parameter-set
        // splice — see `splice_before_first_idr`'s own comment.
        expected.extend_from_slice(&[0, 0, 1]);
        expected.extend_from_slice(&idr);
        assert_eq!(out.payload(), expected.as_slice());
    }

    #[test]
    fn a_leading_non_vcl_unit_stays_ahead_of_the_splice() {
        let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
        let pps = [0x68, 0xEB];
        let sei = [0x06, 0x01, 0x02];
        let idr = [0x65, 0x88];
        let params = h264_params(avcc(&sps, &pps));
        let mut f = (DESC.build)(&params).unwrap();

        f.send_packet(Some(&lp_pkt(&[&sei, &idr]))).unwrap();
        let out = f.receive_packet().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sei);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&pps);
        // 3 bytes, not 4: same reason as the previous test.
        expected.extend_from_slice(&[0, 0, 1]);
        expected.extend_from_slice(&idr);
        assert_eq!(out.payload(), expected.as_slice());
    }

    #[test]
    fn an_inter_frame_gets_no_splice() {
        let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
        let pps = [0x68, 0xEB];
        let p_slice = [0x41, 0x9A, 0x02];
        let params = h264_params(avcc(&sps, &pps));
        let mut f = (DESC.build)(&params).unwrap();

        f.send_packet(Some(&lp_pkt(&[&p_slice]))).unwrap();
        let out = f.receive_packet().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&p_slice);
        assert_eq!(out.payload(), expected.as_slice());
    }

    #[test]
    fn an_already_annexb_stream_passes_through_unchanged() {
        let annexb = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88];
        let params = CodecParameters::video().with_codec(CodecId::H264);
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(
            &Packet::from_slice(&mut budget(), &annexb).unwrap(),
        ))
        .unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &annexb);
    }

    #[test]
    fn an_unsupported_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Hevc);
        assert!((DESC.build)(&params).is_err());
    }

    /// Decode a hex string into bytes. Test-only; not meant to be fast.
    fn from_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0))
            .collect()
    }

    /// A genuine reference oracle, not a self-comparison: `AVCC`, the
    /// length-prefixed packet, and the filtered output are all bytes measured
    /// directly from `ffmpeg 8.1` (`testsrc` -> `libx264` -> MP4, first
    /// packet, `-bsf:v h264_mp4toannexb` on the MP4-sourced stream) — see the
    /// module docs' recipe. The one known, disclosed divergence (a 3-byte
    /// Annex B start code where this filter writes 4) is accounted for
    /// explicitly rather than silently tolerated: `REF` has the reference's
    /// own 1541 bytes, and the assertion checks this filter's output against
    /// `REF` with exactly one `0x00` re-inserted at the documented position,
    /// which is the whole of the difference.
    #[test]
    fn matches_the_reference_on_a_real_mp4_sourced_packet() {
        let avcc = from_hex(
            "0164000affe100186764000aacd94426c044000003000400000300c8\
             3c48965801000668ebe3cb22c0fdf8f800",
        );
        let packet_hex = "000002ac0605ffffa8dc45e9bde6d948b7962cd820d923eeef78323634202d20636f7265203136352072333232322062333536303561202d20482e3236342f4d5045472d342041564320636f646563202d20436f70796c65667420323030332d32303235202d20687474703a2f2f7777772e766964656f6c616e2e6f72672f783236342e68746d6c202d206f7074696f6e733a2063616261633d31207265663d33206465626c6f636b3d313a303a3020616e616c7973653d3078333a3078313133206d653d686578207375626d653d37207073793d31207073795f72643d312e30303a302e3030206d697865645f7265663d31206d655f72616e67653d3136206368726f6d615f6d653d31207472656c6c69733d31203878386463743d312063716d3d3020646561647a6f6e653d32312c313120666173745f70736b69703d31206368726f6d615f71705f6f66667365743d2d3220746872656164733d32206c6f6f6b61686561645f746872656164733d3120736c696365645f746872656164733d30206e723d3020646563696d6174653d3120696e7465726c616365643d3020626c757261795f636f6d7061743d3020636f6e73747261696e65645f696e7472613d3020626672616d65733d3320625f707972616d69643d3220625f61646170743d3120625f626961733d30206469726563743d3120776569676874623d31206f70656e5f676f703d3020776569676874703d32206b6579696e743d3235206b6579696e745f6d696e3d32207363656e656375743d343020696e7472615f726566726573683d302072635f6c6f6f6b61686561643d32352072633d637266206d62747265653d31206372663d32332e302071636f6d703d302e36302071706d696e3d302071706d61783d3639207170737465703d342069705f726174696f3d312e34302061713d313a312e303000800000032c658884009fed73c34a71fe0b59d8fe028f8277372435dbe9cef45d61c52e08a2f4c0f3d5ce2007af7794c0ebda19a4d4905e6a78dea52c2f1e49ec79d8035e6a2685193801e3add3a4d02cd9b79d0a2afa0735b20b9e3cfe6c349bc4ff11b944c9026f6cd7a1dd5358843ad47a5fc5849456201fccb8d44550ca26adb9871ff9f933079b25a6dcf494d70678c4106ae2b299ef671386b1380e135b501fff7b490f9c29542e2038f1852855252728f437866636d7c5ba9fb712fb3819388e7839a5dddfb00daae9040ee87693684ce752c8d2b84b08c8006de8d0f26dea8e062d6175de6549acbc4a45f91ed431f57c0047e233ad233aacceb18a4e657e2f5b284ac130447b38745c35aeeda7a612102c2f5ae6d091350766785ab1ade65d8069579b3cc1ac255d5cf80400600410c77d7002f310331395058153536fbe9faf57cf574754ef5167d553574b0aeec0c3e2e6e6c0ae864c1f3524ba00fed747a05d87199f77c06a360dcffc600184a0ec6f5122fb48a0d68ef2eabf80384793d256e9529a450b51c3fb0ffe571c5f94078e47c961b60de74d394e3e1bcec327256b2acc85852663a4feeb0464309987ac3f5358537c9e7b6726bade391d3ec2e6ccce637e0d9cb7e4ea46cac05920d1a96237961b34d87f072637f2fde7747531342780f186ac7f7d2e3b641588757e0b6ff617f388d644ae9fe2ea95dedeba5ffdb6229731df3bbb99b6fdef6944f75a8a85648c6832eecae8902ea77627ca32b3229f2c0797ac6ec4e196069ca9eeb4318bfccfb4a6385278290456658930bc987dd9cb69dbce8772b8e57958687b1c1be9130a2e92394140e0fcc71ad2c94dea5c4e88fc83de6c96f770fd8efd447fe70b867fad9e511069f0d12f8e4adf2aa201614209f7cf8770bac18adf40db30fd0a2f86cdf850cd134c996282ea1a0dd11da76a3c8663e1147b313ff5f07a76f2c3377c85a441c359cbef712adc65b2f5c3d4d7dfa93f0026b307c020dd5cadd409945abbfe354ba3c1f85233323cb8c945028b6ebeb7bc3866a3308cf7d544466c1226341c7aa8f98855184d61f2fa77907e1d253ddf30e0958f0e2d9807ca645d333cc676c36cd6c641938f6f98c75a82794c2201be4a9b232c9fe9";
        let ref_hex = "000000010605ffffa8dc45e9bde6d948b7962cd820d923eeef78323634202d20636f7265203136352072333232322062333536303561202d20482e3236342f4d5045472d342041564320636f646563202d20436f70796c65667420323030332d32303235202d20687474703a2f2f7777772e766964656f6c616e2e6f72672f783236342e68746d6c202d206f7074696f6e733a2063616261633d31207265663d33206465626c6f636b3d313a303a3020616e616c7973653d3078333a3078313133206d653d686578207375626d653d37207073793d31207073795f72643d312e30303a302e3030206d697865645f7265663d31206d655f72616e67653d3136206368726f6d615f6d653d31207472656c6c69733d31203878386463743d312063716d3d3020646561647a6f6e653d32312c313120666173745f70736b69703d31206368726f6d615f71705f6f66667365743d2d3220746872656164733d32206c6f6f6b61686561645f746872656164733d3120736c696365645f746872656164733d30206e723d3020646563696d6174653d3120696e7465726c616365643d3020626c757261795f636f6d7061743d3020636f6e73747261696e65645f696e7472613d3020626672616d65733d3320625f707972616d69643d3220625f61646170743d3120625f626961733d30206469726563743d3120776569676874623d31206f70656e5f676f703d3020776569676874703d32206b6579696e743d3235206b6579696e745f6d696e3d32207363656e656375743d343020696e7472615f726566726573683d302072635f6c6f6f6b61686561643d32352072633d637266206d62747265653d31206372663d32332e302071636f6d703d302e36302071706d696e3d302071706d61783d3639207170737465703d342069705f726174696f3d312e34302061713d313a312e30300080000000016764000aacd94426c044000003000400000300c83c4896580000000168ebe3cb22c0000001658884009fed73c34a71fe0b59d8fe028f8277372435dbe9cef45d61c52e08a2f4c0f3d5ce2007af7794c0ebda19a4d4905e6a78dea52c2f1e49ec79d8035e6a2685193801e3add3a4d02cd9b79d0a2afa0735b20b9e3cfe6c349bc4ff11b944c9026f6cd7a1dd5358843ad47a5fc5849456201fccb8d44550ca26adb9871ff9f933079b25a6dcf494d70678c4106ae2b299ef671386b1380e135b501fff7b490f9c29542e2038f1852855252728f437866636d7c5ba9fb712fb3819388e7839a5dddfb00daae9040ee87693684ce752c8d2b84b08c8006de8d0f26dea8e062d6175de6549acbc4a45f91ed431f57c0047e233ad233aacceb18a4e657e2f5b284ac130447b38745c35aeeda7a612102c2f5ae6d091350766785ab1ade65d8069579b3cc1ac255d5cf80400600410c77d7002f310331395058153536fbe9faf57cf574754ef5167d553574b0aeec0c3e2e6e6c0ae864c1f3524ba00fed747a05d87199f77c06a360dcffc600184a0ec6f5122fb48a0d68ef2eabf80384793d256e9529a450b51c3fb0ffe571c5f94078e47c961b60de74d394e3e1bcec327256b2acc85852663a4feeb0464309987ac3f5358537c9e7b6726bade391d3ec2e6ccce637e0d9cb7e4ea46cac05920d1a96237961b34d87f072637f2fde7747531342780f186ac7f7d2e3b641588757e0b6ff617f388d644ae9fe2ea95dedeba5ffdb6229731df3bbb99b6fdef6944f75a8a85648c6832eecae8902ea77627ca32b3229f2c0797ac6ec4e196069ca9eeb4318bfccfb4a6385278290456658930bc987dd9cb69dbce8772b8e57958687b1c1be9130a2e92394140e0fcc71ad2c94dea5c4e88fc83de6c96f770fd8efd447fe70b867fad9e511069f0d12f8e4adf2aa201614209f7cf8770bac18adf40db30fd0a2f86cdf850cd134c996282ea1a0dd11da76a3c8663e1147b313ff5f07a76f2c3377c85a441c359cbef712adc65b2f5c3d4d7dfa93f0026b307c020dd5cadd409945abbfe354ba3c1f85233323cb8c945028b6ebeb7bc3866a3308cf7d544466c1226341c7aa8f98855184d61f2fa77907e1d253ddf30e0958f0e2d9807ca645d333cc676c36cd6c641938f6f98c75a82794c2201be4a9b232c9fe9";
        let packet = from_hex(packet_hex);
        let reference = from_hex(ref_hex);

        let params = h264_params(avcc);
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(&Packet::from_slice(&mut budget(), &packet).unwrap()))
            .unwrap();
        let ours = f.receive_packet().unwrap().payload().to_vec();

        // No widening needed any more: `splice_before_first_idr` now writes
        // the same 3-byte start code the reference does immediately before
        // the spliced-in IDR slice, so this is a plain byte-exact
        // comparison against real `ffmpeg`'s own output.
        assert_eq!(
            ours, reference,
            "differs from ffmpeg 8.1's own h264_mp4toannexb output"
        );
    }
}
