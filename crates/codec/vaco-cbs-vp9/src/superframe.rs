//! The VP9 superframe index, Annex B of the VP9 Bitstream & Decoding Process
//! Specification (v0.6) — split and reassemble.
//!
//! A superframe packs several coded frames (typically a hidden alt-ref ahead
//! of the visible frame it references) into one container sample, with an
//! index appended so a demuxer-side reader can split them without decoding
//! anything: one marker byte, `frame_count` fixed-width sizes, and a second
//! copy of the marker byte.
//!
//! This is a second, small implementation of the same algorithm
//! `vaco-parse-vpx::superframe::last_subframe` already has — see this crate's
//! own module doc for why a shared implementation was not possible here.
//! Unlike that function (which only needs the *last* sub-frame), this one
//! needs every sub-frame's range, to split a sample into that many
//! [`vaco_codec_cbs::CbsUnit`]s.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// The superframe marker's top-3-bit pattern, §Annex B.
const MARKER_PATTERN: u8 = 0xC0;
const MARKER_MASK: u8 = 0xE0;

/// The byte ranges of every sub-frame in `data`, in bitstream order — or
/// `None` when `data` carries no superframe index at all, in which case the
/// whole buffer is one frame.
///
/// # Errors
///
/// [`Error::InvalidData`] when the last byte looks like a superframe marker
/// but the index it describes is inconsistent (wrong leading copy, sizes that
/// do not exactly fill the region ahead of the index) — a caller should treat
/// this the same as "malformed input", not silently fall back to "one frame",
/// which would hand a decoder the index bytes as if they were coded data.
pub fn sub_frame_ranges(data: &[u8], budget: &mut Budget) -> Result<Option<Vec<(usize, usize)>>> {
    let Some(&marker) = data.last() else {
        return Ok(None);
    };
    if marker & MARKER_MASK != MARKER_PATTERN {
        return Ok(None);
    }
    let bytes_per_size = usize::from((marker >> 3) & 0x3) + 1;
    let frame_count = usize::from(marker & 0x7) + 1;
    let index_size = 2usize
        .checked_add(
            bytes_per_size
                .checked_mul(frame_count)
                .ok_or(Error::InvalidData("vp9 superframe index: size overflow"))?,
        )
        .ok_or(Error::InvalidData("vp9 superframe index: size overflow"))?;
    if index_size > data.len() {
        // Not enough room for an index this marker describes: this is an
        // ordinary frame whose last byte happens to look like a marker.
        return Ok(None);
    }
    let index_start = data.len() - index_size;
    if data.get(index_start) != Some(&marker) {
        return Ok(None);
    }
    let Some(sizes_region) = data.get(index_start + 1..data.len() - 1) else {
        return Ok(None);
    };
    let mut ranges: Vec<(usize, usize)> = budget.alloc(frame_count)?;
    ranges.clear();
    let mut offset = 0usize;
    for chunk in sizes_region.chunks(bytes_per_size) {
        if chunk.len() != bytes_per_size {
            return Err(Error::InvalidData("vp9 superframe index: truncated size"));
        }
        let mut size = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            size |= u64::from(b) << (8 * i);
        }
        let size = usize::try_from(size)
            .map_err(|_| Error::InvalidData("vp9 superframe index: size too large"))?;
        let start = offset;
        offset = offset
            .checked_add(size)
            .ok_or(Error::InvalidData("vp9 superframe index: size overflow"))?;
        ranges.push((start, offset));
    }
    if offset != index_start {
        return Err(Error::InvalidData(
            "vp9 superframe index: sizes do not fill the coded region",
        ));
    }
    Ok(Some(ranges))
}

/// Write a superframe index over `frame_lens`, appending it to `out`.
///
/// Picks the narrowest `bytes_per_size` that holds the largest frame — the
/// natural, minimal encoding, though not necessarily the same width the
/// original encoder chose if this is reassembling a stream that was split
/// and edited (the same category of framing choice
/// `vaco_parse_av1::cbs::Av1Cbs::assemble` documents for its own
/// `frame_unit` grouping).
///
/// # Errors
///
/// [`Error::InvalidData`] when there are too many frames, or one frame, to
/// index at all (§Annex B allows 1..=8 frames; a single frame never needs an
/// index, so this only ever writes one for 2..=8).
pub fn write_index(out: &mut Vec<u8>, budget: &mut Budget, frame_lens: &[usize]) -> Result<()> {
    if !(2..=8).contains(&frame_lens.len()) {
        return Err(Error::InvalidData(
            "vp9 superframe index: needs 2..=8 frames",
        ));
    }
    let max_len = frame_lens.iter().copied().max().unwrap_or(0) as u64;
    let bytes_per_size = if max_len < (1 << 8) {
        1
    } else if max_len < (1 << 16) {
        2
    } else if max_len < (1 << 24) {
        3
    } else {
        4
    };
    let index_len = 2 + bytes_per_size * frame_lens.len();
    budget.check(index_len as u64)?;
    let marker =
        MARKER_PATTERN | (((bytes_per_size - 1) as u8) << 3) | (frame_lens.len() - 1) as u8;
    out.push(marker);
    for &len in frame_lens {
        let len = len as u64;
        for i in 0..bytes_per_size {
            out.push(((len >> (8 * i)) & 0xFF) as u8);
        }
    }
    out.push(marker);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    /// Built by hand from Annex B's own layout — `libvpx-vp9` did not emit a
    /// visible multi-frame superframe under any setting this environment's
    /// `ffmpeg` build exposed a knob for, the same wall
    /// `vaco-parse-vpx::superframe`'s own tests document hitting.
    fn built(sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        for &s in sizes {
            out.extend(std::iter::repeat_n(0xAAu8, s));
        }
        let marker = 0xC0 | (sizes.len() as u8 - 1);
        out.push(marker);
        for &s in sizes {
            out.push(s as u8);
        }
        out.push(marker);
        out
    }

    #[test]
    fn a_two_frame_index_yields_both_ranges() {
        let buf = built(&[10, 20]);
        let mut budget = Budget::new(Limits::strict());
        let ranges = sub_frame_ranges(&buf, &mut budget)
            .expect("no error")
            .expect("an index");
        assert_eq!(ranges, vec![(0, 10), (10, 30)]);
    }

    #[test]
    fn a_plain_frame_has_no_index() {
        let buf = [0x82u8, 0x49, 0x83, 0x42, 0, 0];
        let mut budget = Budget::new(Limits::strict());
        assert_eq!(sub_frame_ranges(&buf, &mut budget).expect("no error"), None);
    }

    #[test]
    fn a_short_buffer_never_panics() {
        let mut budget = Budget::new(Limits::strict());
        assert_eq!(sub_frame_ranges(&[], &mut budget).expect("no error"), None);
        assert_eq!(
            sub_frame_ranges(&[0xC0], &mut budget).expect("no error"),
            None
        );
    }

    #[test]
    fn writing_and_reading_an_index_round_trips() {
        let lens = [100usize, 250, 30];
        let mut out = vec![0xAAu8; 100];
        out.extend(std::iter::repeat_n(0xBBu8, 250));
        out.extend(std::iter::repeat_n(0xCCu8, 30));
        let mut budget = Budget::new(Limits::strict());
        write_index(&mut out, &mut budget, &lens).expect("writes");
        let ranges = sub_frame_ranges(&out, &mut budget)
            .expect("no error")
            .expect("an index");
        assert_eq!(ranges, vec![(0, 100), (100, 350), (350, 380)]);
    }

    #[test]
    fn one_frame_is_refused_since_it_never_needs_an_index() {
        let mut budget = Budget::new(Limits::strict());
        assert!(write_index(&mut Vec::new(), &mut budget, &[10]).is_err());
    }

    #[test]
    fn wide_sizes_pick_a_wider_byte_count() {
        let lens = [70_000usize, 10];
        let mut out = vec![0u8; 70_010];
        let mut budget = Budget::new(Limits::strict());
        write_index(&mut out, &mut budget, &lens).expect("writes");
        let ranges = sub_frame_ranges(&out, &mut budget)
            .expect("no error")
            .expect("an index");
        assert_eq!(ranges, vec![(0, 70_000), (70_000, 70_010)]);
    }
}
