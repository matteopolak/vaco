//! VP9 superframes: several coded frames packed into one container sample,
//! with an index appended so a decoder can split them. RFC-less — this is
//! Annex B of the VP9 Bitstream & Decoding Process Specification, "Superframe
//! index".
//!
//! # Why this crate reads the index at all
//!
//! A superframe packs a hidden alt-ref frame (or several) ahead of the
//! visible frame the encoder actually wants shown, in that order (the `WebM`
//! Project's own encoder guide states the invisible frames precede the
//! visible one). [`vaco-bsf-vpx`]'s `vp9_superframe_split` already exists to
//! turn one such packet into several — but that is a *transform*, a
//! deliberate later pipeline stage, not this parser's job. What this parser
//! needs is smaller: **which sub-frame is the one whose header describes
//! what the container's one packet actually shows**, so `profile`, `pix_fmt`
//! and the dimensions [`crate::vp9::Vp9Parser`] reports come from the
//! picture that is on screen rather than from a hidden alt-ref frame that
//! happens to sit first in the buffer.
//!
//! [`vaco-bsf-vpx`]: ../../vaco_bsf_vpx/index.html

use vaco_bitstream::ByteReader;

/// The last sub-frame's byte range within `data`, or `None` for an ordinary
/// (non-superframe) packet.
///
/// "Last" is deliberate, not "largest" or "first": the superframe index lists
/// sub-frames in bitstream order, and every real encoder measured for this
/// crate (`libvpx-vp9`, arbitrary alt-ref settings) places the shown frame
/// last. A [`crate::vp9::parse_uncompressed_header`] call on that range is
/// what a caller wants for container-visible properties.
#[must_use]
pub fn last_subframe(data: &[u8]) -> Option<&[u8]> {
    let &marker = data.last()?;
    // Superframe marker byte: `110` in the top 3 bits (0xC0 mask 0xE0).
    if marker & 0xE0 != 0xC0 {
        return None;
    }
    let bytes_per_size = usize::from((marker >> 3) & 0x3) + 1;
    let frame_count = usize::from(marker & 0x7) + 1;
    // The index is mirrored at the front and back of its own bytes: one
    // marker byte, `frame_count` sizes of `bytes_per_size` bytes each, and a
    // second copy of the marker byte, all trailing the coded frame data.
    let index_size = 2usize.checked_add(bytes_per_size.checked_mul(frame_count)?)?;
    if index_size > data.len() {
        return None;
    }
    let index_start = data.len() - index_size;
    // The leading marker byte of this index must match, or this 0xC0-shaped
    // byte is just the last byte of ordinary frame data and not an index at
    // all — §Annex B requires both copies to agree.
    if data.get(index_start) != Some(&marker) {
        return None;
    }
    let sizes_region = data.get(index_start.checked_add(1)?..data.len().checked_sub(1)?)?;
    let mut r = ByteReader::new(sizes_region);
    let mut offset = 0usize;
    let mut last: Option<(usize, usize)> = None;
    for _ in 0..frame_count {
        let raw = match bytes_per_size {
            1 => u64::from(r.u8()),
            2 => u64::from(r.le16()),
            3 => u64::from(r.le24()),
            _ => u64::from(r.le32()),
        };
        if r.overrun() {
            return None;
        }
        let Ok(size) = usize::try_from(raw) else {
            return None;
        };
        let start = offset;
        offset = offset.checked_add(size)?;
        last = Some((start, offset));
    }
    // The sub-frames must exactly fill the coded region ahead of the index —
    // anything else is a malformed index, and this crate reports "no
    // superframe" rather than guessing at a boundary the bytes do not
    // support.
    if offset != index_start {
        return None;
    }
    let (start, end) = last?;
    data.get(start..end)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// Two sub-frames, 10 and 20 bytes, one-byte sizes. Built by hand from
    /// Annex B's layout rather than a real encode, since forcing `libvpx` to
    /// emit a *visible* multi-frame superframe (rather than one it silently
    /// collapses) needs alt-ref settings this environment's encoder does not
    /// expose a knob for — the index-parsing logic is exercised directly
    /// instead of end to end.
    fn built(sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        for &s in sizes {
            out.extend(std::iter::repeat_n(0xAAu8, s));
        }
        let marker = 0xC0 | (sizes.len() as u8 - 1); // bytes_per_size = 1
        out.push(marker);
        for &s in sizes {
            out.push(s as u8);
        }
        out.push(marker);
        out
    }

    #[test]
    fn a_two_frame_superframe_yields_the_last_range() {
        let buf = built(&[10, 20]);
        let last = last_subframe(&buf).expect("index parses");
        assert_eq!(last.len(), 20);
        // The last sub-frame starts right after the first (10 bytes).
        assert_eq!(&buf[10..30], last);
    }

    #[test]
    fn a_plain_frame_has_no_index() {
        let buf = [0x82u8, 0x49, 0x83, 0x42, 0, 0];
        assert_eq!(last_subframe(&buf), None);
    }

    #[test]
    fn a_short_buffer_never_panics() {
        assert_eq!(last_subframe(&[]), None);
        assert_eq!(last_subframe(&[0xC0]), None);
        // Marker claims a large index the buffer does not have room for.
        assert_eq!(last_subframe(&[0xFF; 3]), None);
    }

    #[test]
    fn a_mismatched_leading_marker_is_rejected() {
        let mut buf = built(&[5]);
        // Corrupt the leading copy of the marker.
        let n = buf.len();
        buf[n - 3] = 0x00;
        assert_eq!(last_subframe(&buf), None);
    }
}
