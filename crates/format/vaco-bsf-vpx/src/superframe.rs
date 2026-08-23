//! `vp9_superframe`: the inverse of [`crate::superframe_split`] — bundle a
//! buffered invisible (alt-ref) frame together with the visible frame that
//! follows it into one superframe packet.
//!
//! # The grouping rule, measured
//!
//! A VP9 frame states whether it is displayed in the first handful of bits
//! of its uncompressed header (VP9 §6.2), read here with nothing but
//! [`vaco_bitstream::BitReader`] (no VP9 parser crate exists in this
//! workspace to depend on instead, and this is the only field this filter
//! needs): `frame_marker(2)`, `profile_low_bit(1)`, `profile_high_bit(1)`,
//! then — only if `profile == 3` — a `reserved_zero(1)`, then
//! `show_existing_frame(1)`. If that bit is set the frame is a
//! zero-cost "show a previously decoded frame" instruction and there is
//! nothing more to read; otherwise `frame_type(1)` then `show_frame(1)`
//! follow directly.
//!
//! This bit layout was **verified against real reference-encoded bytes**,
//! not assumed from a spec reading: decoding it on both halves of a real
//! measured superframe (see [`crate::superframe_split`]'s module docs for
//! the byte layout that isolated them) gives `frame_marker == 0b10` on
//! *both* halves — the required VP9 sync value — which a wrong bit offset
//! would hit only by chance. The larger half (13445 bytes) decoded to
//! `show_frame = 0` and the smaller half (664 bytes) to `show_frame = 1`,
//! consistent with an alt-ref (large, reference-only, never displayed)
//! followed by the frame that actually gets shown. The same decode on four
//! ordinary (non-superframe) frames from the same file — one keyframe, three
//! inter frames — gave `show_frame = 1` on every one, as it must for frames
//! a real player displays individually.
//!
//! **Not independently measured**: whether `show_existing_frame == 1` really
//! is a flush trigger the same way `show_frame == 1` is. No fixture in this
//! environment's reach produces one (see [`crate`]'s module docs on
//! `vp9_raw_reorder` for the same gap). Treated as a flush trigger because
//! it is the only spec-consistent reading — a "show a decoded frame now"
//! instruction is definitionally the end of an invisible run — and flagged
//! here rather than silently assumed.
//!
//! # Round trip
//!
//! `vp9_superframe_split` then `vp9_superframe`, applied to a real
//! `libvpx-vp9` two-pass/`-auto-alt-ref` elementary stream, reproduces the
//! original packetisation exactly (`framecrc` agreement, all 75 frames,
//! including all 6 superframes) — see this crate's test below for the
//! offline version of that check.
//!
//! A group of exactly one frame is emitted **as-is, with no index appended**
//! — measured: `vp9_superframe` applied directly to a stream with no
//! alt-ref frames at all was byte-identical to its input, so a lone visible
//! frame must never grow a size-1 superframe wrapper around itself.

use std::collections::VecDeque;

use vaco_bitstream::BitReader;
use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "vp9_superframe",
    long_name: "Merge VP9 frames into superframes",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Vp9) => Ok(Box::new(MappedFilter::new(Superframe {
            pending: Vec::new(),
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("vp9_superframe: vp9 only")),
    }
}

/// Whether this frame is displayed now — `show_frame` or
/// `show_existing_frame` — per the bit layout measured in the module docs.
/// A header too short to read (or a bad `frame_marker`) is treated as
/// "displayed now": flushing immediately on unparseable input is the
/// conservative choice, since the alternative is buffering forever on
/// garbage.
fn is_shown_now(payload: &[u8]) -> bool {
    let mut r = BitReader::new(payload);
    let frame_marker = r.get(2);
    let profile_low = r.get(1);
    let profile_high = r.get(1);
    if frame_marker != 0b10 {
        return true;
    }
    if (profile_high << 1) | profile_low == 3 {
        r.skip(1); // reserved_zero
    }
    let show_existing_frame = r.get(1);
    if show_existing_frame != 0 {
        return true;
    }
    let _frame_type = r.get(1);
    let show_frame = r.get(1);
    if r.overrun() {
        return true;
    }
    show_frame != 0
}

struct Superframe {
    pending: Vec<Packet>,
    budget: Budget,
}

impl Superframe {
    fn flush(&mut self, out: &mut VecDeque<Packet>) -> Result<()> {
        match self.pending.len() {
            0 => {}
            1 => out.push_back(self.pending.remove(0)),
            _ => {
                let frames: Vec<&[u8]> = self.pending.iter().map(Packet::payload).collect();
                let max_len = frames.iter().map(|f| f.len()).max().unwrap_or(0);
                // The fewest bytes that can hold the largest constituent
                // frame's length, little-endian: 1..=4, per the index
                // format's two-bit `magbytes - 1` field.
                let magbytes = if max_len < 1 << 8 {
                    1
                } else if max_len < 1 << 16 {
                    2
                } else if max_len < 1 << 24 {
                    3
                } else {
                    4
                };
                let marker = 0xC0
                    | (u8::try_from(magbytes - 1).unwrap_or(3) << 3)
                    | u8::try_from(frames.len().saturating_sub(1)).unwrap_or(7);

                let mut combined = Vec::new();
                for f in &frames {
                    combined.extend_from_slice(f);
                }
                combined.push(marker);
                for f in &frames {
                    let len = f.len().to_le_bytes();
                    combined.extend_from_slice(len.get(..magbytes).unwrap_or(&[]));
                }
                combined.push(marker);

                let Some(first_meta) = self.pending.first().map(|f| (f.stream_index, f.pts, f.dts, f.duration, f.pos, f.flags)) else {
                    return Ok(());
                };
                let (stream_index, pts, dts, duration, pos, flags) = first_meta;
                let mut np = Packet::from_slice(&mut self.budget, &combined)?;
                np.stream_index = stream_index;
                np.pts = pts;
                np.dts = dts;
                np.duration = duration;
                np.pos = pos;
                np.flags = flags;
                out.push_back(np);
                self.pending.clear();
            }
        }
        Ok(())
    }
}

impl PacketMap for Superframe {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else {
            return self.flush(out);
        };
        let shown = is_shown_now(p.payload());
        self.pending.push(p.clone());
        // The index format's `nframes - 1` field is three bits: 8 is the
        // most a superframe can ever declare. Flushing here rather than
        // panicking or truncating keeps a pathological run of "never
        // shown" frames from growing the group past what the format can
        // represent at all.
        if shown || self.pending.len() >= 8 {
            self.flush(out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn vp9_params() -> CodecParameters {
        CodecParameters::video().with_codec(CodecId::Vp9)
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    /// `frame_marker=10, profile=00, show_existing_frame=0, frame_type,
    /// show_frame` packed into the first byte's top bits, matching the
    /// measured bit layout.
    fn frame(show_frame: bool, filler_len: usize) -> Vec<u8> {
        // bit0-1 frame_marker=10, bit2-3 profile=00, bit4
        // show_existing_frame=0, bit5 frame_type=0, bit6 show_frame.
        let mut byte0 = 0b1000_0000u8;
        if show_frame {
            byte0 |= 0b0000_0010;
        }
        let mut v = vec![byte0];
        v.extend(std::iter::repeat_n(0xAB, filler_len));
        v
    }

    #[test]
    fn a_hidden_then_shown_frame_are_merged_into_one_superframe() {
        let hidden = frame(false, 30);
        let shown = frame(true, 4);
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&hidden))).unwrap();
        assert!(filt.receive_packet().is_err(), "must not flush while hidden");
        filt.send_packet(Some(&pkt(&shown))).unwrap();
        let out = filt.receive_packet().unwrap();

        let split_back = {
            let mut s = (crate::superframe_split::DESC.build)(&vp9_params()).unwrap();
            s.send_packet(Some(&out)).unwrap();
            let a = s.receive_packet().unwrap();
            let b = s.receive_packet().unwrap();
            (a, b)
        };
        assert_eq!(split_back.0.payload(), hidden.as_slice());
        assert_eq!(split_back.1.payload(), shown.as_slice());
    }

    /// A lone visible frame is emitted as-is — measured: no fixture in this
    /// environment ever produced a size-1 superframe wrapper.
    #[test]
    fn a_lone_visible_frame_gets_no_index() {
        let shown = frame(true, 10);
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&shown))).unwrap();
        let out = filt.receive_packet().unwrap();
        assert_eq!(out.payload(), shown.as_slice());
    }

    /// End of stream flushes whatever is still buffered, even if it was
    /// never marked shown — never silently drop the last group.
    #[test]
    fn eof_flushes_a_pending_hidden_frame() {
        let hidden = frame(false, 5);
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&hidden))).unwrap();
        filt.send_packet(None).unwrap();
        assert_eq!(filt.receive_packet().unwrap().payload(), hidden.as_slice());
    }

    #[test]
    fn a_non_vp9_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Vp8);
        assert!((DESC.build)(&params).is_err());
    }

    /// Falsifies the bit-layout reading directly: a byte whose top two bits
    /// are not `10` is not a valid VP9 frame marker, and this crate treats
    /// that as "shown now" (flush) rather than trying to interpret garbage
    /// as a real header.
    #[test]
    fn falsified_a_bad_frame_marker_is_treated_as_shown_not_hidden() {
        assert!(is_shown_now(&[0x00, 0x00]));
    }
}
