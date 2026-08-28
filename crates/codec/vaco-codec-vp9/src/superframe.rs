//! Annex B — Superframes. Unlike `vaco-parse-vpx::superframe::last_subframe`
//! (which only needs the *last* sub-frame, for container-visible metadata),
//! a decoder must decode **every** sub-frame in order — a superframe's
//! leading entries are typically hidden alt-ref frames that become
//! reference frames for the visible one that follows, so skipping them
//! would decode the wrong picture even where this crate's scope (key
//! frames only) applies to each individual sub-frame.

/// Every sub-frame's byte range within `data`, in bitstream order. Returns
/// a single range covering the whole buffer when `data` does not end in a
/// superframe index (the common case: one packet, one frame).
#[must_use]
pub fn split(data: &[u8]) -> Vec<&[u8]> {
    if let Some(ranges) = try_split(data) {
        return ranges.into_iter().filter_map(|(s, e)| data.get(s..e)).collect();
    }
    vec![data]
}

fn try_split(data: &[u8]) -> Option<Vec<(usize, usize)>> {
    let &marker = data.last()?;
    if marker & 0xE0 != 0xC0 {
        return None;
    }
    let bytes_per_size = usize::from((marker >> 3) & 0x3) + 1;
    let frame_count = usize::from(marker & 0x7) + 1;
    let index_size = 2usize.checked_add(bytes_per_size.checked_mul(frame_count)?)?;
    if index_size > data.len() {
        return None;
    }
    let index_start = data.len() - index_size;
    if data.get(index_start) != Some(&marker) {
        return None;
    }
    let sizes_region = data.get(index_start.checked_add(1)?..data.len().checked_sub(1)?)?;
    let mut ranges = Vec::new();
    let mut offset = 0usize;
    for chunk in sizes_region.chunks(bytes_per_size) {
        if chunk.len() != bytes_per_size {
            return None;
        }
        let mut raw: u64 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            raw |= u64::from(b) << (8 * i);
        }
        let size = usize::try_from(raw).ok()?;
        let start = offset;
        offset = offset.checked_add(size)?;
        ranges.push((start, offset));
    }
    if ranges.len() != frame_count || offset != index_start {
        return None;
    }
    Some(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn splits_every_subframe_in_order() {
        let buf = built(&[10, 20, 5]);
        let parts = split(&buf);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts.first().map(|p| p.len()), Some(10));
        assert_eq!(parts.get(1).map(|p| p.len()), Some(20));
        assert_eq!(parts.get(2).map(|p| p.len()), Some(5));
    }

    #[test]
    fn a_plain_frame_is_one_part() {
        let buf = [0x82u8, 0x49, 0x83, 0x42, 0, 0];
        let parts = split(&buf);
        assert_eq!(parts, vec![&buf[..]]);
    }

    #[test]
    fn malformed_input_never_panics() {
        // Not a superframe index, so each falls back to "the whole buffer
        // is one frame" -- even an empty buffer, which the header parser
        // downstream rejects on its own terms rather than this module
        // guessing at "nothing to decode" here.
        assert_eq!(split(&[]), vec![&[] as &[u8]]);
        assert_eq!(split(&[0xC0]).len(), 1);
        assert_eq!(split(&[0xFF; 3]).len(), 1);
    }
}
