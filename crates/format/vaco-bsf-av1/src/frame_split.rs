//! `av1_frame_split`: split a packet carrying more than one coded frame into
//! one packet per frame.
//!
//! # What is measured, not assumed
//!
//! `ffmpeg -h bsf=av1_frame_split` says only `Supported codecs: av1` — no
//! options, no prose. Its actual grouping rule was measured against
//! `ffmpeg 8.1` on an SVT-AV1-encoded `testsrc` clip read back through the
//! `obu` demuxer (`planning/CONFORMANCE-FINDINGS.md`-style black-box probing,
//! D17), by dumping every OBU's type and size per packet with a `framecrc`
//! comparison before and after the filter:
//!
//! * A packet with exactly **one** frame-bearing OBU (`OBU_FRAME` or
//!   `OBU_FRAME_HEADER`) is passed through **unchanged** — measured on a
//!   temporal unit shaped `TD, SEQUENCE_HEADER, FRAME`, which stayed one
//!   1023-byte packet.
//! * A packet with **several** frame-bearing OBUs is split into one packet
//!   per frame. Every OBU *before* the first frame-bearing one — the
//!   temporal delimiter, a sequence header, metadata — is **prepended to the
//!   first output packet**, not emitted on its own. Measured on a temporal
//!   unit shaped `TD, FRAME, FRAME, FRAME, FRAME, FRAME` (241 bytes): the
//!   output was five packets of 109, 71, 24, 19, 18 bytes — 109 is exactly
//!   the 2-byte TD plus the first 107-byte FRAME OBU, and the remaining four
//!   are the bare FRAME OBUs unchanged. All five packets carried the
//!   **input packet's own `pts`/`dts`**, unmodified — none were renumbered.
//!
//! `OBU_TILE_GROUP` continues the frame unit its preceding `OBU_FRAME_HEADER`
//! opened, per the AV1 spec's frame/tile-group pairing (§7.4) — not
//! independently measured (no fixture in this crate's test corpus splits a
//! `FRAME_HEADER` from its tile groups; SVT-AV1 always emits the combined
//! `OBU_FRAME` type), but the only reading consistent with a decoder needing
//! a frame's tile data delivered with it. `OBU_REDUNDANT_FRAME_HEADER` is
//! treated as opening a new frame unit the same way `OBU_FRAME_HEADER` does,
//! for the same unmeasured-but-only-consistent-reading reason. Both are
//! flagged here, not silently assumed, in case a future measurement
//! disagrees.
//!
//! # `dovi_rpu` is not in this crate
//!
//! `ffmpeg -h bsf=dovi_rpu` reports `Supported codecs: hevc av1` — genuinely
//! dual-codec, unlike everything else in `-bsfs` with `av1` in its supported
//! list. It does not fit `vaco-bsf-av1` (AV1-only) or `vaco-bsf-h2645`
//! (H.264/HEVC's *_mp4toannexb shape) cleanly, Dolby Vision RPU parsing is
//! substantial on its own, and issue #351 names only `vaco-bsf-av1`/`-vpx` as
//! its crates — so this is a deliberate omission, not an oversight, left for
//! a `vaco-bsf-dovi` (or similar) crate of its own.
//!
//! # Malformed input
//!
//! If the OBUs in a packet's payload do not account for every byte (a
//! truncated tail, or bytes that fail to parse as OBUs at all —
//! [`vaco_parse_av1::obu::units`] stops at the first such unit), this filter
//! passes the packet through **unchanged** rather than splitting a partial
//! read and silently dropping the remainder. Not a reference behaviour this
//! crate could measure (every real encoder output parses cleanly); a
//! conservative default that never loses bytes.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_av1::obu::{self, ObuType};
use vaco_parse_av1::Av1Framing;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "av1_frame_split",
    long_name: "Split AV1 frames into single OBUs",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Av1) => Ok(Box::new(MappedFilter::new(FrameSplit {
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("av1_frame_split: av1 only")),
    }
}

struct FrameSplit {
    budget: Budget,
}

/// Whether `t` opens a new frame unit — see the module docs for which two of
/// these are measured and which two are the only spec-consistent reading.
fn opens_frame_unit(t: ObuType) -> bool {
    t == ObuType::FRAME || t == ObuType::FRAME_HEADER || t == ObuType::REDUNDANT_FRAME_HEADER
}

impl PacketMap for FrameSplit {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let payload = p.payload();
        let units = obu::units(payload, Av1Framing::ObuStream);

        let consumed: usize = units.iter().map(|u| u.total_len).sum();
        let frame_units = units.iter().filter(|u| opens_frame_unit(u.header.obu_type)).count();
        if consumed != payload.len() || frame_units <= 1 {
            // Nothing to split: either the payload did not parse cleanly
            // (pass through rather than risk losing bytes), or there is at
            // most one frame here already.
            out.push_back(p.clone());
            return Ok(());
        }

        // `group_start` begins at 0 so any leading non-frame OBUs (the
        // temporal delimiter, a sequence header) ride along with whichever
        // group is first. Only the *second and later* frame-opening unit
        // starts a fresh group — the first one just marks that a group is
        // now open.
        let mut group_start = 0usize;
        let mut seen_first_frame_unit = false;
        for unit in &units {
            if opens_frame_unit(unit.header.obu_type) {
                if seen_first_frame_unit {
                    let group = payload.get(group_start..unit.offset).unwrap_or(&[]);
                    out.push_back(clone_with_payload(p, group, &mut self.budget)?);
                    group_start = unit.offset;
                }
                seen_first_frame_unit = true;
            }
        }
        let last = payload.get(group_start..).unwrap_or(&[]);
        out.push_back(clone_with_payload(p, last, &mut self.budget)?);
        Ok(())
    }
}

/// A packet with `p`'s metadata (timestamps, flags, stream index) and
/// `payload` as its bytes — the same "everything but the bytes" copy every
/// filter in `vaco-bsf-generic` does.
fn clone_with_payload(p: &Packet, payload: &[u8], budget: &mut Budget) -> Result<Packet> {
    let mut np = Packet::from_slice(budget, payload)?;
    np.stream_index = p.stream_index;
    np.pts = p.pts;
    np.dts = p.dts;
    np.duration = p.duration;
    np.pos = p.pos;
    np.flags = p.flags;
    Ok(np)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn av1_params() -> CodecParameters {
        CodecParameters::video().with_codec(CodecId::Av1)
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn annexb_pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    /// One OBU: header byte `(type << 3) | 0b10` (`has_size_field` set, no
    /// extension), a one-byte leb128 size, then `payload`.
    fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![(obu_type << 3) | 0b0000_0010];
        v.push(payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }

    /// The measured shape: `TD, SEQ_HDR, FRAME` — one frame, must not split.
    #[test]
    fn a_single_frame_temporal_unit_is_untouched() {
        let mut buf = obu(2, &[]); // TD
        buf.extend(obu(1, &[0xAA])); // SEQ_HDR
        buf.extend(obu(6, &[0xBB, 0xCC])); // FRAME
        let mut f = (DESC.build)(&av1_params()).unwrap();
        f.send_packet(Some(&annexb_pkt(&buf))).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(out.payload(), buf.as_slice());
        assert!(f.receive_packet().is_err(), "must be exactly one output packet");
    }

    /// The measured shape: `TD, FRAME, FRAME, FRAME` — the TD rides with the
    /// first FRAME, and every later FRAME is its own packet.
    #[test]
    fn multiple_frames_are_split_with_the_td_folded_into_the_first() {
        let td = obu(2, &[]);
        let f1 = obu(6, &[0x01]);
        let f2 = obu(6, &[0x02, 0x03]);
        let f3 = obu(6, &[0x04]);
        let mut buf = td.clone();
        buf.extend(&f1);
        buf.extend(&f2);
        buf.extend(&f3);

        let mut filt = (DESC.build)(&av1_params()).unwrap();
        filt.send_packet(Some(&annexb_pkt(&buf))).unwrap();

        let p0 = filt.receive_packet().unwrap();
        let mut expected0 = td.clone();
        expected0.extend(&f1);
        assert_eq!(p0.payload(), expected0.as_slice());

        let p1 = filt.receive_packet().unwrap();
        assert_eq!(p1.payload(), f2.as_slice());

        let p2 = filt.receive_packet().unwrap();
        assert_eq!(p2.payload(), f3.as_slice());

        assert!(filt.receive_packet().is_err());
    }

    /// Every split packet carries the input packet's own timestamps — none
    /// are renumbered. Measured: five split packets all kept the parent's
    /// `pts`/`dts`.
    #[test]
    fn split_packets_keep_the_parents_timestamps() {
        let f1 = obu(6, &[0x01]);
        let f2 = obu(6, &[0x02]);
        let mut buf = f1.clone();
        buf.extend(&f2);
        let mut pkt = annexb_pkt(&buf);
        pkt.pts = vaco_core::Timestamp::new(48_000);
        pkt.dts = vaco_core::Timestamp::new(48_000);

        let mut filt = (DESC.build)(&av1_params()).unwrap();
        filt.send_packet(Some(&pkt)).unwrap();
        let a = filt.receive_packet().unwrap();
        let b = filt.receive_packet().unwrap();
        assert_eq!(a.pts, pkt.pts);
        assert_eq!(b.pts, pkt.pts);
        assert_eq!(a.dts, pkt.dts);
        assert_eq!(b.dts, pkt.dts);
    }

    #[test]
    fn a_non_av1_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::H264);
        assert!((DESC.build)(&params).is_err());
    }

    #[test]
    fn truncated_input_is_passed_through_rather_than_partially_split() {
        let mut buf = obu(6, &[0x01, 0x02]);
        buf.truncate(buf.len() - 1); // cut the last payload byte off
        let mut filt = (DESC.build)(&av1_params()).unwrap();
        filt.send_packet(Some(&annexb_pkt(&buf))).unwrap();
        assert_eq!(filt.receive_packet().unwrap().payload(), buf.as_slice());
    }

    /// Falsifies the "always split, TD gets its own packet" naive reading:
    /// that would produce 4 packets from the 3-FRAME-plus-TD fixture above,
    /// not 3, and the first would be TD alone rather than TD+FRAME.
    #[test]
    fn falsified_a_standalone_td_packet_would_be_the_wrong_count() {
        let td = obu(2, &[]);
        let f1 = obu(6, &[0x01]);
        let f2 = obu(6, &[0x02]);
        let f3 = obu(6, &[0x03]);
        let mut buf = td;
        buf.extend(&f1);
        buf.extend(&f2);
        buf.extend(&f3);
        let mut filt = (DESC.build)(&av1_params()).unwrap();
        filt.send_packet(Some(&annexb_pkt(&buf))).unwrap();
        let mut count = 0;
        while filt.receive_packet().is_ok() {
            count += 1;
        }
        assert_eq!(count, 3, "TD does not get a packet of its own");
    }
}
