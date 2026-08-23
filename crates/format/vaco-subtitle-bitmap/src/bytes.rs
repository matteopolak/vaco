//! Bounds-checked big-endian field reads, shared by every segment/record
//! parser in this crate.
//!
//! `clippy::indexing_slicing` is denied workspace-wide, so a fixed-width
//! header field is read through `.get()` rather than `[..]`, exactly the
//! idiom `vaco-format-core::probe::ProbeData` and `vaco-demux-mpegps::pes`
//! already use — this is that same idiom over a plain `&[u8]` rather than a
//! `ProbeData`, for the places this crate reads fields at demux time rather
//! than at probe time.

/// Big-endian `u16` at `at`, or `None` if it runs past `b`.
pub(crate) fn rb16(b: &[u8], at: usize) -> Option<u16> {
    let hi = *b.get(at)?;
    let lo = *b.get(at.checked_add(1)?)?;
    Some(u16::from_be_bytes([hi, lo]))
}

/// Big-endian `u32` at `at`, or `None` if it runs past `b`.
pub(crate) fn rb32(b: &[u8], at: usize) -> Option<u32> {
    let a = *b.get(at)?;
    let b1 = *b.get(at.checked_add(1)?)?;
    let c = *b.get(at.checked_add(2)?)?;
    let d = *b.get(at.checked_add(3)?)?;
    Some(u32::from_be_bytes([a, b1, c, d]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rb16_reads_big_endian() {
        assert_eq!(rb16(&[0x01, 0x02], 0), Some(0x0102));
    }

    #[test]
    fn rb16_none_past_the_end() {
        assert_eq!(rb16(&[0x01], 0), None);
        assert_eq!(rb16(&[0x01, 0x02], 1), None);
    }

    #[test]
    fn rb32_reads_big_endian() {
        assert_eq!(rb32(&[0x01, 0x02, 0x03, 0x04], 0), Some(0x0102_0304));
    }

    #[test]
    fn rb32_none_past_the_end() {
        assert_eq!(rb32(&[0x01, 0x02, 0x03], 0), None);
    }

    #[test]
    fn reads_never_panic_at_the_usize_boundary() {
        assert_eq!(rb16(&[0, 0], usize::MAX), None);
        assert_eq!(rb32(&[0, 0, 0, 0], usize::MAX), None);
    }
}
