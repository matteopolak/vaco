//! `vp9_extract_vpcc`: synthesise a `vpcC`-shaped configuration record from
//! a VP9 frame header, for a container arriving with none.
//!
//! # The gap this closes
//!
//! `extract_extradata` (`vaco-bsf-generic`) pulls H.264/HEVC parameter sets
//! back out of a bitstream that carries them in-band. VP9 has no equivalent
//! in-band parameter set — `WebM`/Matroska carries no `CodecPrivate` for
//! VP8/VP9 at all (every field a decoder needs comes from each frame's own
//! `uncompressed_header()` instead) — so a `vaco -c copy` remux from
//! Matroska into MP4 handed `vaco-mux-mp4::entry::build_video` an empty
//! `extradata` slice, which `writer::vpcc` wrote through verbatim: a `vpcC`
//! box with a correct 8-byte header and **zero payload bytes**, which real
//! `ffprobe` refuses to open at all (`Empty VP Codec Configuration box`).
//! This filter is the fix: derive the record from the bitstream itself,
//! the same way [`vaco_parse_vpx::vpcc::from_vp9_header`]'s own doc
//! describes, and report it back exactly how `extract_extradata` reports
//! its own findings — a [`PacketSideData::NewExtradata`] on the packet that
//! supplied it.
//!
//! # VP8 is deliberately not covered
//!
//! RFC 6386's key-frame header states `color_space`/`clamping_type` only
//! inside the boolean-coded first partition (`vaco-parse-vpx::vp8`'s own
//! module doc) — genuinely unreachable without running the arithmetic
//! decoder, not a gap this crate chose to leave. Real `ffmpeg 9.0.1` does
//! not support VP8-in-MP4 at all (measured: `-c copy` on a real
//! `libvpx`-encoded `WebM` refuses outright, "codec not currently supported
//! in container"), so there is no reference behaviour to match by guessing
//! either. `vaco-mux-mp4::entry::build_video` refuses a `vp08` entry with
//! empty extradata by name instead — a refusal beats a box with a fabricated
//! colour bit nothing could check.
//!
//! # Why a key frame's own header is enough
//!
//! §6.2's `color_config()` is read before any arithmetic-coded data at all,
//! on every key frame and every profile-1..3 intra-only frame — no decode
//! needed, only the same header walk [`vaco_parse_vpx::vp9::Vp9Header`]
//! already does for probing. An ordinary inter frame carries no
//! `color_config()` (the module doc for [`vaco_parse_vpx::vp9`] explains
//! why), so this filter simply waits: a real encoder's first frame is
//! always a key frame, and a mid-stream inter frame reaching this filter
//! before one has already failed to mux for other reasons.
//!
//! # Emitted once per change, matching `extract_extradata`
//!
//! The first frame that yields a record attaches it; a later key frame only
//! attaches a new one if its own record's bytes actually differ (a VP9
//! stream changing resolution or bit depth mid-file, however rare).

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData};
use vaco_parse_vpx::vp9;
use vaco_parse_vpx::vpcc;
use vaco_pool::Buffer;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "vp9_extract_vpcc",
    long_name: "Derive a vpcC configuration record from VP9 frame headers",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Vp9) => Ok(Box::new(MappedFilter::new(ExtractVpcc {
            stored: Vec::new(),
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("vp9_extract_vpcc: vp9 only")),
    }
}

struct ExtractVpcc {
    /// The `vpcC` payload last attached as `NewExtradata`, so a later frame
    /// whose derived record has not changed emits nothing.
    stored: Vec<u8>,
    budget: Budget,
}

impl PacketMap for ExtractVpcc {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };

        let mut out_pkt = p.clone();
        if let Some(header) = vp9::parse_display_header(p.payload())
            && let Some(rec) = vpcc::from_vp9_header(&header)
        {
            let candidate = vpcc::build(&rec).to_vec();
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
    use vaco_limits::{Budget, Limits};

    /// Build a profile-0 `uncompressed_header()` bit for bit, per §6.2 — the
    /// same technique `vaco-parse-vpx::vp9`'s own test module uses, kept
    /// local since that helper is private to its crate.
    fn key_frame_bits(cs: u8, full_range: bool, width: u32, height: u32) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let mut push = |n: u32, v: u32| {
            for i in (0..n).rev() {
                bits.push((v >> i) & 1 != 0);
            }
        };
        push(2, 2); // frame_marker
        push(1, 0); // profile_low
        push(1, 0); // profile_high
        push(1, 0); // show_existing_frame
        push(1, 0); // frame_type = KEY_FRAME
        push(1, 1); // show_frame
        push(1, 0); // error_resilient_mode
        push(8, 0x49);
        push(8, 0x83);
        push(8, 0x42);
        // profile 0: bit_depth is not coded (always 8).
        push(3, u32::from(cs));
        if cs != 7 {
            push(1, u32::from(full_range));
        }
        push(16, width - 1);
        push(16, height - 1);
        let mut out = Vec::new();
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                if b {
                    byte |= 0x80 >> i;
                }
            }
            out.push(byte);
        }
        out
    }

    #[test]
    fn build_refuses_non_vp9() {
        assert!(build(&CodecParameters::video().with_codec(CodecId::Vp8)).is_err());
    }

    #[test]
    fn a_frame_with_no_parseable_header_emits_nothing() {
        let mut f = ExtractVpcc {
            stored: Vec::new(),
            budget: Budget::new(Limits::strict()),
        };
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[0, 0, 0]).unwrap();
        let mut out = VecDeque::new();
        f.push(Some(&pkt), &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].side_data.is_empty());
    }

    /// The real bug this filter fixes: a key frame with no container-level
    /// config record must still come out with a non-empty, correctly-shaped
    /// `vpcC` payload attached as `NewExtradata`.
    #[test]
    fn a_key_frame_yields_a_non_empty_vpcc() {
        let mut f = ExtractVpcc {
            stored: Vec::new(),
            budget: Budget::new(Limits::strict()),
        };
        let mut budget = Budget::new(Limits::strict());
        let data = key_frame_bits(0, false, 64, 64);
        let pkt = Packet::from_slice(&mut budget, &data).unwrap();
        let mut out = VecDeque::new();
        f.push(Some(&pkt), &mut out).unwrap();
        assert_eq!(out.len(), 1);
        let side = out[0].side_data.first().unwrap();
        let PacketSideData::NewExtradata(buf) = side else {
            unreachable!("wrong side-data kind");
        };
        assert_eq!(buf.len(), 12, "a vpcC payload is always 12 bytes");
        let rec = vpcc::parse(buf.as_slice()).unwrap();
        assert_eq!(rec.profile, 0);
        assert_eq!(rec.bit_depth, 8);
    }

    /// The same record twice in a row (the overwhelming common case) emits
    /// `NewExtradata` only once, matching `extract_extradata`'s own
    /// "emitted once per change" contract.
    #[test]
    fn an_unchanged_record_is_not_re_emitted() {
        let mut f = ExtractVpcc {
            stored: Vec::new(),
            budget: Budget::new(Limits::strict()),
        };
        let mut budget = Budget::new(Limits::strict());
        let data = key_frame_bits(0, false, 64, 64);
        let pkt = Packet::from_slice(&mut budget, &data).unwrap();
        let mut out = VecDeque::new();
        f.push(Some(&pkt), &mut out).unwrap();
        f.push(Some(&pkt), &mut out).unwrap();
        assert!(!out[0].side_data.is_empty());
        assert!(out[1].side_data.is_empty());
    }
}
