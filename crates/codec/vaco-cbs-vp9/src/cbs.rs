//! [`vaco_codec_cbs::CbsCodec`] for VP9: [`Vp9Cbs`] splits a container sample
//! into its coded frames (via the superframe index, when one is present),
//! and decodes/encodes each frame's [`crate::header::Vp9Header`].
//!
//! # Content carries the header **and** the opaque tail
//!
//! Unlike H.264/HEVC/AV1's `Content` enums, [`Vp9Content`] is not just the
//! typed header: it pairs the header with `tail`, the compressed header and
//! tile data that follow it, copied verbatim. There is nowhere else for those
//! bytes to live — [`vaco_codec_cbs::CbsUnit::data`] is replaced wholesale on
//! a write, so a `Content` that dropped the tail would have no way to put it
//! back.

use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::header::Vp9Header;
use crate::superframe::{sub_frame_ranges, write_index};

/// VP9 has exactly one framing shape at this crate's granularity: a
/// container sample, optionally a superframe wrapping several coded frames.
/// There is no second associated-type variant to name (unlike H.26x's Annex
/// B versus length-prefixed, or AV1's OBU-stream versus low-overhead), so
/// this is a unit struct purely to satisfy [`CbsCodec::Framing`]'s shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vp9Framing;

/// One coded frame's content: its typed header, and everything after the
/// header's own byte-aligned end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vp9Content {
    pub header: Vp9Header,
    /// The compressed header and tile data, or (for
    /// [`Vp9Header::ShowExistingFrame`]) nothing — that variant's header is
    /// the whole frame.
    pub tail: Vec<u8>,
}

/// The VP9 [`CbsCodec`]. Holds nothing: there is no cross-frame state this
/// crate's syntax needs (no escaping to undo, no parameter-set store — VP9
/// carries no persistent parameter sets at all, only per-frame headers).
#[derive(Debug, Default, Clone, Copy)]
pub struct Vp9Cbs;

impl Vp9Cbs {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CbsCodec for Vp9Cbs {
    type Content = Vp9Content;
    type Framing = Vp9Framing;
    const NAME: &'static str = "vp9";

    fn split(
        &self,
        data: &[u8],
        _framing: Vp9Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        match sub_frame_ranges(data, budget)? {
            Some(ranges) => {
                for (start, end) in ranges {
                    let bytes = data.get(start..end).ok_or(Error::InvalidData(
                        "vp9 superframe index named a range outside the buffer",
                    ))?;
                    // No per-unit "type" exists at this granularity — every
                    // sub-frame is the same kind of thing syntactically, and
                    // what distinguishes a key frame from an inter frame is
                    // a field *inside* the header, not a framing-level tag.
                    fragment.push(CbsUnit::new(0, bytes.to_vec()), budget)?;
                }
            }
            None => {
                fragment.push(CbsUnit::new(0, data.to_vec()), budget)?;
            }
        }
        Ok(())
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        _framing: Vp9Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        let total: u64 = fragment.units().iter().map(|u| u.data.len() as u64).sum();
        budget.check(total)?;
        match fragment.len() {
            0 => Ok(()),
            1 => {
                if let Some(u) = fragment.units().first() {
                    out.extend_from_slice(&u.data);
                }
                Ok(())
            }
            _ => {
                let lens: Vec<usize> = fragment.units().iter().map(|u| u.data.len()).collect();
                for u in fragment.units() {
                    out.extend_from_slice(&u.data);
                }
                write_index(out, budget, &lens)
            }
        }
    }

    fn read_unit(&mut self, unit: &CbsUnit, _budget: &mut Budget) -> Result<Vp9Content> {
        let (header, end) = Vp9Header::parse(&unit.data)?;
        let tail = unit.data.get(end..).unwrap_or(&[]).to_vec();
        Ok(Vp9Content { header, tail })
    }

    fn write_unit(
        &mut self,
        content: &Vp9Content,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        let header_bytes = content.header.write();
        budget.check((header_bytes.len() + content.tail.len()) as u64)?;
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(&content.tail);
        Ok(())
    }

    fn content_unit_type(&self, _content: &Vp9Content) -> u32 {
        0
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_cbs::Cbs;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// A real `libvpx-vp9` key frame, `ffmpeg -f lavfi -i
    /// testsrc2=size=176x144:rate=10 -c:v libvpx-vp9 -pix_fmt yuv420p -crf 30
    /// -b:v 0 -g 5`, first IVF frame payload, in full (3943 bytes) — long
    /// enough to include real tile data past the header, unlike
    /// `header.rs`'s truncated prefixes.
    fn real_key_frame() -> Vec<u8> {
        include_bytes!("../tests/fixtures/real_key_frame.bin").to_vec()
    }

    #[test]
    fn a_plain_sample_splits_into_one_unit() {
        let data = real_key_frame();
        let mut cbs = Cbs::new(Vp9Cbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&data, Vp9Framing, &mut f, &mut b).expect("splits");
        assert_eq!(f.len(), 1);
        f.release(&mut b);
    }

    /// The real key frame's header and tail round-trip byte for byte with no
    /// edit — the whole 3943-byte frame, tile data included, not just the
    /// header prefix `header.rs`'s own tests check.
    #[test]
    fn a_real_key_frame_round_trips_bit_exactly_with_no_edit() {
        let data = real_key_frame();
        let mut cbs = Cbs::new(Vp9Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Vp9Framing, &mut f, &mut b).expect("splits");
        let content = cbs.read_unit(&f, 0, &mut b).expect("reads");
        assert!(matches!(content.header, Vp9Header::Frame(_)));
        let before = f.units()[0].data.clone();
        cbs.update_unit(&mut f, 0, &content, &mut b).expect("rewrites");
        assert_eq!(f.units()[0].data, before, "re-encodes identically");
        f.release(&mut b);
    }

    /// A field edit through the typed header changes only that field.
    #[test]
    fn editing_a_typed_field_changes_only_that_field() {
        let data = real_key_frame();
        let mut cbs = Cbs::new(Vp9Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Vp9Framing, &mut f, &mut b).expect("splits");
        let mut content = cbs.read_unit(&f, 0, &mut b).expect("reads");
        let Vp9Header::Frame(fh) = &mut content.header else {
            panic!("expected a coded frame");
        };
        let original_size = (fh.width, fh.height);
        fh.loop_filter.level = 30;
        cbs.update_unit(&mut f, 0, &content, &mut b).expect("rewrites");

        let reread = cbs.read_unit(&f, 0, &mut b).expect("re-reads");
        let Vp9Header::Frame(fh) = &reread.header else {
            panic!("expected a coded frame");
        };
        assert_eq!(fh.loop_filter.level, 30, "the edited field stuck");
        assert_eq!((fh.width, fh.height), original_size, "nothing else moved");
        f.release(&mut b);
    }

    /// A hand-built two-frame superframe (real header bytes, arbitrary tile
    /// payload — the split/reassemble logic does not care what the tile
    /// bytes are) splits into two units and reassembles byte for byte.
    #[test]
    fn a_hand_built_superframe_splits_and_reassembles() {
        let frame_a = vec![0xAAu8; 40];
        let frame_b = vec![0xBBu8; 25];
        let mut data = frame_a.clone();
        data.extend_from_slice(&frame_b);
        let marker = 0xC0u8 | 1; // bytes_per_size = 1, frame_count = 2
        data.push(marker);
        data.push(40);
        data.push(25);
        data.push(marker);

        let mut cbs = Cbs::new(Vp9Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Vp9Framing, &mut f, &mut b).expect("splits");
        assert_eq!(f.len(), 2);
        assert_eq!(f.units()[0].data, frame_a);
        assert_eq!(f.units()[1].data, frame_b);

        let mut out = Vec::new();
        cbs.assemble(&f, Vp9Framing, &mut out, &mut b).expect("assembles");
        assert_eq!(out, data);
        f.release(&mut b);
    }

    #[test]
    fn every_truncation_splits_and_reads_without_panicking() {
        let data = real_key_frame();
        let mut cbs = Cbs::new(Vp9Cbs::new());
        let mut b = budget();
        for n in (0..data.len()).step_by(37) {
            let mut f = CbsFragment::new();
            let _ = cbs.split(&data[..n], Vp9Framing, &mut f, &mut b);
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            f.release(&mut b);
        }
    }
}
