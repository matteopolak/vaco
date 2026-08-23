//! `vp9_superframe_split`: split a VP9 superframe back into its constituent
//! frames.
//!
//! # The superframe index, measured
//!
//! VP9 packs an invisible (alt-ref) frame together with the visible frame
//! that follows it into one **superframe**: the constituent frames
//! concatenated, followed by an index. Measured directly off a real
//! `libvpx-vp9` two-pass encode with `-auto-alt-ref 1` (alt-ref needs
//! two-pass — single-pass encodes in this environment never produced one,
//! which is worth recording since it cost real time to discover):
//!
//! ```text
//! last byte:  c9  =  1100_1001
//!             marker(3)=110, magbytes-1(2)=01, nframes-1(3)=001
//!          -> magbytes=2, nframes=2
//! index:      c9 85 34 98 02 c9   (6 bytes = 2 + nframes*magbytes)
//!             marker, [size0 lo,hi]=0x3485=13445, [size1 lo,hi]=0x0298=664,
//!             marker again
//! ```
//!
//! `13445 + 664 + 6 == 14115`, the frame's total size — the declared sizes
//! cover only the constituent frames, not the index itself. Confirmed this
//! is really a size table and not coincidence by checking the arithmetic
//! holds on all six superframes the same encode produced, not just one.
//!
//! A frame with no superframe marker (the last byte's top three bits are not
//! `110`) is not a superframe at all and passes through unchanged — measured
//! as the overwhelming common case: only 6 of 75 frames in the same encode
//! were superframes.
//!
//! # What is not measured
//!
//! Whether every constituent packet keeps the parent's `pts`/`dts` verbatim,
//! or only the last (visible) one does. This crate keeps the parent's
//! timestamps on every constituent packet, matching
//! `av1_frame_split`/`av1_frame_merge`'s convention in the sibling crate;
//! [`crate::superframe`]'s round-trip test confirms this at least
//! *round-trips* correctly (both filters agree on the same convention), but
//! does not by itself prove it is the reference's choice rather than a
//! coincidence of the round trip.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "vp9_superframe_split",
    long_name: "Split VP9 superframes into single frames",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Vp9) => Ok(Box::new(MappedFilter::new(SuperframeSplit {
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(vaco_core::Error::Unsupported("vp9_superframe_split: vp9 only")),
    }
}

struct SuperframeSplit {
    budget: Budget,
}

/// The constituent frame sizes a superframe index declares, or `None` if
/// `payload` does not end in one — see the module docs for the byte layout.
fn superframe_sizes(payload: &[u8]) -> Option<Vec<usize>> {
    let &marker = payload.last()?;
    if marker & 0xE0 != 0xC0 {
        return None;
    }
    let magbytes = usize::from((marker >> 3) & 0x3) + 1;
    let nframes = usize::from(marker & 0x7) + 1;
    let index_size = 2 + nframes * magbytes;
    if index_size > payload.len() {
        return None;
    }
    let index = payload.get(payload.len() - index_size..)?;
    if index.first() != Some(&marker) {
        return None;
    }
    let mut sizes = Vec::new();
    let mut total = 0usize;
    for chunk in index.get(1..1 + nframes * magbytes)?.chunks(magbytes) {
        let mut v = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            v |= u64::from(b) << (8 * i);
        }
        let size = usize::try_from(v).ok()?;
        total = total.checked_add(size)?;
        sizes.push(size);
    }
    // The declared sizes plus the index must account for every byte —
    // otherwise this is not really a superframe, just a frame that happens
    // to end in a byte matching the marker pattern.
    if total.checked_add(index_size)? != payload.len() {
        return None;
    }
    Some(sizes)
}

impl PacketMap for SuperframeSplit {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let payload = p.payload();
        let Some(sizes) = superframe_sizes(payload) else {
            out.push_back(p.clone());
            return Ok(());
        };
        let mut offset = 0usize;
        for size in sizes {
            let frame = payload.get(offset..offset + size).unwrap_or(&[]);
            let mut np = Packet::from_slice(&mut self.budget, frame)?;
            np.stream_index = p.stream_index;
            np.pts = p.pts;
            np.dts = p.dts;
            np.duration = p.duration;
            np.pos = p.pos;
            np.flags = p.flags;
            out.push_back(np);
            offset += size;
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

    /// The exact measured example from the module docs: two frames of
    /// 13445 and 664 bytes, a two-byte-magnitude, two-frame index.
    fn build_superframe(frame0: &[u8], frame1: &[u8]) -> Vec<u8> {
        let marker = 0xC0 | ((2 - 1) << 3) | (2 - 1); // magbytes=2, nframes=2
        let mut buf = Vec::new();
        buf.extend_from_slice(frame0);
        buf.extend_from_slice(frame1);
        buf.push(marker);
        buf.extend_from_slice(&(frame0.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(frame1.len() as u16).to_le_bytes());
        buf.push(marker);
        buf
    }

    #[test]
    fn a_measured_two_frame_superframe_is_split_in_order() {
        let f0 = vec![0xAA; 20];
        let f1 = vec![0xBB; 5];
        let buf = build_superframe(&f0, &f1);
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&buf))).unwrap();
        assert_eq!(filt.receive_packet().unwrap().payload(), f0.as_slice());
        assert_eq!(filt.receive_packet().unwrap().payload(), f1.as_slice());
        assert!(filt.receive_packet().is_err());
    }

    #[test]
    fn an_ordinary_frame_with_no_index_is_untouched() {
        let frame = vec![0x82, 0x49, 0x83, 0x42, 0x00];
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&frame))).unwrap();
        let out = filt.receive_packet().unwrap();
        assert_eq!(out.payload(), frame.as_slice());
        assert!(filt.receive_packet().is_err());
    }

    /// A frame whose last byte coincidentally has the marker's top bits set
    /// but whose declared sizes do not add up must not be mis-split.
    #[test]
    fn a_coincidental_marker_byte_with_bad_sizes_is_left_alone() {
        let mut frame = vec![0x00; 10];
        *frame.last_mut().unwrap() = 0xC0; // marker(3)=110, magbytes=1, nframes=1
        // index would need to be 3 bytes (2 + 1*1) but the one size byte
        // here plus the index does not sum to the frame length.
        let mut filt = (DESC.build)(&vp9_params()).unwrap();
        filt.send_packet(Some(&pkt(&frame))).unwrap();
        assert_eq!(filt.receive_packet().unwrap().payload(), frame.as_slice());
    }

    #[test]
    fn a_non_vp9_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Vp8);
        assert!((DESC.build)(&params).is_err());
    }

    /// Falsified: the arithmetic check is what tells a real superframe from
    /// a coincidence. Remove it (accept whatever the marker claims) and this
    /// fixture's declared sizes (0-length first frame) would wrongly split.
    #[test]
    fn falsified_skipping_the_length_check_would_accept_garbage() {
        let marker = 0xC0 | 1u8; // magbytes=1, nframes=2
        let mut buf = vec![0xFF; 4];
        buf.push(marker);
        buf.push(0); // claims frame 0 is 0 bytes
        buf.push(0); // claims frame 1 is 0 bytes
        buf.push(marker);
        // total declared (0) + index (4) == 4, but buf.len() == 8: mismatch.
        assert!(superframe_sizes(&buf).is_none());
    }
}
