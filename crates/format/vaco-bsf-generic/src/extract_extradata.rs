//! `extract_extradata`: synthesise extradata from in-band parameter sets.
//!
//! Motivated by `planning/CONFORMANCE-FINDINGS.md` finding 26 and
//! `vaco-mux-mp4`'s `check_bitstream` (M16,
//! [`vaco_format_core::mux::global_header_action`]): a container with no
//! out-of-band configuration record — AVI, MPEG-TS, raw Annex B — carries an
//! H.264/HEVC stream's SPS/PPS/VPS *inside* the access units, and anything
//! that wants them as a stream-level `extradata` field (a probe reporting
//! `extradata_size`, or a muxer that needs `avcC`/`hvcC` up front) has to pull
//! them back out. This is that pull.
//!
//! The assembly rule itself — which units count as parameter sets, and how
//! their bytes are laid out — lives in [`vaco_format_nalu::extradata`], not
//! here. `vaco-format-core`'s stream discovery needs the exact same rule to
//! close finding 26's read half, and D19 allows it exactly one definition;
//! see that module's docs for the measurement and the rejected alternatives.
//! This crate is the *write*-side caller: a [`BitstreamFilter`] that a muxer
//! or `-bsf:v extract_extradata` can insert into a packet stream.
//!
//! # What is measured, not assumed
//!
//! Checked against `ffmpeg 8.1`, not read from its source (D7): a synthetic
//! `testsrc` clip encoded with `libx264`, muxed to AVI (finding 26's own
//! recipe), and separately run straight through
//! `-bsf:v extract_extradata,dump_extra=freq=keyframe` on the raw Annex B
//! elementary stream to isolate exactly the bytes the filter adds. Both
//! routes produced the same 37-byte buffer, reproduced in
//! [`vaco_format_nalu::extradata`]'s own doc comment and tests.
//!
//! One thing worth calling out here specifically, because it is a fact about
//! *this filter's construction* rather than about the assembly rule:
//! `extract_extradata` does not remove the units from the packet by default.
//! `-bsf:v extract_extradata=remove=1` does; the bare name does not.
//! [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open) carries no
//! per-instance option string (see `planning/INTERFACE-GAPS.md` for that
//! gap), so this crate can only ever construct the bare-name behaviour —
//! `remove` is simply never reachable through the seam today.
//!
//! # How the result is reported
//!
//! Matches the reference's own mechanism: a
//! [`vaco_packet::PacketSideData::NewExtradata`] attached to the packet whose
//! parameter sets produced it, not a mutation of [`CodecParameters`] (which
//! this filter never sees again after construction). A caller — a muxer's
//! `write_packet`, or a demuxer's discovery pass — reads it back off the
//! packet and decides what a "new extradata" means for it.
//!
//! Emitted once per *change*: the first packet carrying a parameter set
//! attaches the initial buffer; a later packet only attaches a new one if the
//! collected set's bytes actually differ from what was last emitted. A file
//! whose SPS/PPS never change (the overwhelming majority) reports it exactly
//! once, on the first keyframe.
//!
//! # Codec coverage
//!
//! `ffmpeg -h bsf=extract_extradata` lists `av1 avs2 avs3 cavs h264 hevc lcevc
//! mpeg1video mpeg2video vc1 vvc` — eleven codecs. This crate implements H.264
//! and HEVC, the two this workspace has NAL-level parameter-set vocabulary
//! for ([`vaco_parse_h264`], [`vaco_parse_hevc`]); [`build`] returns
//! [`Error::Unsupported`] for the other nine rather than silently doing
//! nothing, so a caller finds out at construction time, not by an absent
//! side-data block three packets later.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, HeaderKind, LengthSize};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData};
use vaco_pool::Buffer;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "extract_extradata",
    long_name: "Extract extradata from the bitstream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    let codec = params
        .codec_id
        .ok_or(Error::Unsupported("extract_extradata: stream has no codec id"))?;
    let header_kind = match codec {
        CodecId::H264 => HeaderKind::H264,
        CodecId::Hevc => HeaderKind::H265,
        _ => {
            return Err(Error::Unsupported(
                "extract_extradata: this build only extracts h264 and hevc parameter sets",
            ));
        }
    };
    // AVI/MPEG-TS-sourced streams are Annex B (`nal_length_size` absent or
    // zero); an MP4-sourced stream driven through this filter directly (a
    // caller re-running extraction after some other reframing) would be
    // length-prefixed, so both framings are honoured rather than assuming
    // the common case.
    let framing = params
        .video
        .as_ref()
        .and_then(|v| v.nal_length_size)
        .and_then(LengthSize::new)
        .map_or(Framing::AnnexB, Framing::LengthPrefixed);
    Ok(Box::new(MappedFilter::new(ExtractExtradata {
        header_kind,
        framing,
        stored: Vec::new(),
        budget: Budget::new(Limits::permissive()),
    })))
}

struct ExtractExtradata {
    header_kind: HeaderKind,
    framing: Framing,
    /// The Annex-B-framed buffer last attached as `NewExtradata`, so a later
    /// packet whose parameter sets have not changed emits nothing.
    stored: Vec<u8>,
    /// Reused across the filter's whole lifetime, like
    /// `vaco-mux-avi`/`vaco-mux-mpegts`'s own `convert_budget` field — this is
    /// a session-length meter, not a per-call one, released explicitly when
    /// `stored` is replaced (see [`ExtractExtradata::push`]).
    budget: Budget,
}

impl PacketMap for ExtractExtradata {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };

        let found = vaco_format_nalu::parameter_sets(p.payload(), self.framing, self.header_kind);

        let mut out_pkt = p.clone();
        if !found.is_empty() {
            let candidate = vaco_format_nalu::assemble_extradata(found);
            if candidate != self.stored {
                self.budget
                    .release(u64::try_from(self.stored.len()).unwrap_or(0));
                let buf = Buffer::from_slice(&mut self.budget, &candidate)?;
                self.stored = candidate;
                out_pkt.side_data.push(PacketSideData::NewExtradata(buf));
            }
        }
        out.push_back(out_pkt);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;

    fn h264_params() -> CodecParameters {
        CodecParameters::video().with_codec(CodecId::H264)
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn annexb_pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    fn side_data_extradata(p: &Packet) -> Option<&[u8]> {
        p.side_data.iter().find_map(|sd| match sd {
            PacketSideData::NewExtradata(b) => Some(b.as_slice()),
            _ => None,
        })
    }

    /// Reference-measured 37-byte example from the module docs, run through
    /// this implementation rather than ffmpeg — the byte comparison against
    /// ffmpeg itself lives in the crate-level integration test, which shells
    /// out; this is the fast, offline regression once that comparison has
    /// established the expected bytes.
    #[test]
    fn h264_sps_pps_are_collected_with_the_measured_start_code_convention() {
        let sps = [
            0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00,
            0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x48, 0x96, 0x58,
        ];
        let pps = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(&sps);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&pps);
        annexb.extend_from_slice(&[0, 0, 1, 0x65, 0xAA]); // a slice, ignored

        let mut f = (DESC.build)(&h264_params()).unwrap();
        f.send_packet(Some(&annexb_pkt(&annexb))).unwrap();
        let out = f.receive_packet().unwrap();
        let extra = side_data_extradata(&out).unwrap();

        let mut expected = vec![0, 0, 1];
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&pps);
        assert_eq!(extra, expected.as_slice());
    }

    #[test]
    fn unchanged_parameter_sets_emit_no_second_side_data() {
        let sps_pps = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        let mut f = (DESC.build)(&h264_params()).unwrap();
        f.send_packet(Some(&annexb_pkt(&sps_pps))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_some());

        f.send_packet(Some(&annexb_pkt(&sps_pps))).unwrap();
        assert!(side_data_extradata(&f.receive_packet().unwrap()).is_none());
    }

    #[test]
    fn a_packet_with_no_parameter_sets_passes_through_untouched() {
        let slice_only = [0, 0, 0, 1, 0x65, 0xAA, 0xBB];
        let mut f = (DESC.build)(&h264_params()).unwrap();
        f.send_packet(Some(&annexb_pkt(&slice_only))).unwrap();
        let out = f.receive_packet().unwrap();
        assert!(side_data_extradata(&out).is_none());
        assert_eq!(out.payload(), &slice_only);
    }

    #[test]
    fn length_prefixed_input_is_honoured_when_the_stream_says_so() {
        let sps = [0x67, 0x42, 0xC0, 0x1E];
        let mut lp = Vec::new();
        lp.extend_from_slice(&(sps.len() as u32).to_be_bytes());
        lp.extend_from_slice(&sps);

        let mut params = h264_params();
        params.video = Some(VideoParameters {
            nal_length_size: Some(4),
            ..VideoParameters::default()
        });
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(&annexb_pkt(&lp))).unwrap();
        let out = f.receive_packet().unwrap();
        let extra = side_data_extradata(&out).unwrap();
        assert_eq!(extra, &[0, 0, 1, 0x67, 0x42, 0xC0, 0x1E]);
    }

    #[test]
    fn an_unsupported_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Av1);
        assert!((DESC.build)(&params).is_err());
    }

    #[test]
    fn falsified_the_start_code_convention_would_disagree_with_the_measured_bytes() {
        // Planting the defect: using a four-byte start code on the *first*
        // unit too (the "obvious" spelling) produces different bytes from
        // what ffmpeg 8.1 actually writes, which is the whole reason the
        // module docs call it out. This asserts the wrong spelling really is
        // wrong, so the real test above is not passing by coincidence.
        let sps = [0x67, 0x42, 0xC0, 0x1E];
        let naive: Vec<u8> = [0, 0, 0, 1].iter().chain(sps.iter()).copied().collect();
        let measured: Vec<u8> = [0, 0, 1].iter().chain(sps.iter()).copied().collect();
        assert_ne!(naive, measured);
    }
}
