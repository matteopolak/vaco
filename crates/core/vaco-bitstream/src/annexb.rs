//! Annex-B start-code framing and RBSP escaping.
//!
//! Written from ITU-T H.264 (V14, 2020) §7.3.1, §7.4.1.1 and Annex B — the byte
//! stream format, `emulation_prevention_three_byte`, and `trailing_zero_8bits`.

/// Whether any byte of a machine word is zero.
///
/// The classic bit trick: `x - 0x01..01` borrows into the high bit of any byte
/// that was zero, `& !x` keeps only bytes that were zero rather than large, and
/// the mask isolates the flags.
#[inline]
const fn has_zero_byte(w: u64) -> bool {
    w.wrapping_sub(0x0101_0101_0101_0101) & !w & 0x8080_8080_8080_8080 != 0
}

/// Find the next three-byte start code `00 00 01` at or after `from`.
///
/// Returns the index of its first zero byte. A four-byte start code
/// `00 00 00 01` is found at its *second* zero — the leading zero belongs to the
/// preceding unit as `trailing_zero_8bits`, and [`NalIter`] trims it there.
///
/// # Performance
///
/// Two skips compose. The classic three-byte stride: if the byte at `i + 2` is
/// not zero, no start code can begin at `i`, `i + 1` or `i + 2`, so advance
/// three. On top of that, a word with no zero byte at all cannot contain the
/// *first two* bytes of a start code beginning anywhere in its first seven
/// positions, so advance seven. Video payload is overwhelmingly non-zero, so the
/// word skip is what carries most of the scan.
///
/// This is the scalar reference. `vaco-simd` will own a vectorised `scan` once
/// that crate lands; the two must agree exactly, which is what makes this
/// function worth keeping rather than deleting.
#[must_use]
pub fn find_start_code(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    loop {
        if let Some(w) = buf.get(i..).and_then(<[u8]>::first_chunk::<8>)
            && !has_zero_byte(u64::from_ne_bytes(*w))
        {
            i += 7;
            continue;
        }
        let &[a, b, c] = buf.get(i..).and_then(<[u8]>::first_chunk::<3>)?;
        match c {
            0 => i += 1,
            1 if a == 0 && b == 0 => return Some(i),
            _ => i += 3,
        }
    }
}

/// An iterator over the NAL units of an Annex-B byte stream.
///
/// Yields EBSP — the payload as it appears in the stream, emulation-prevention
/// bytes still in place. Call [`to_rbsp`] to remove them.
///
/// Empty units (two adjacent start codes) are skipped rather than yielded, and
/// `trailing_zero_8bits` are trimmed from the end of each unit, so the
/// concatenation of the yielded slices is a subsequence of the input rather than
/// a partition of it.
///
/// The iterator always terminates: every step advances the cursor past at least
/// one start code.
///
/// # Example
///
/// ```
/// use vaco_bitstream::annexb;
///
/// let stream = [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB];
/// let units: Vec<&[u8]> = annexb::nal_units(&stream).collect();
/// assert_eq!(units, vec![&[0x67u8, 0xAA][..], &[0x68, 0xBB][..]]);
/// ```
#[derive(Debug, Clone)]
pub struct NalIter<'a> {
    buf: &'a [u8],
    /// Start of the current unit's payload, or `None` once exhausted.
    next: Option<usize>,
}

/// Iterate the NAL units of an Annex-B byte stream.
#[must_use]
pub fn nal_units(buf: &[u8]) -> NalIter<'_> {
    NalIter {
        buf,
        next: find_start_code(buf, 0).map(|i| i + 3),
    }
}

impl<'a> Iterator for NalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let start = self.next?;
            let (end, next) = match find_start_code(self.buf, start) {
                Some(sc) => (sc, Some(sc + 3)),
                None => (self.buf.len(), None),
            };
            self.next = next;
            let unit = self.buf.get(start..end).unwrap_or(&[]);
            // `trailing_zero_8bits`, which also absorbs the leading zero of a
            // four-byte start code.
            let trimmed = match unit.iter().rposition(|&b| b != 0) {
                Some(last) => unit.get(..=last).unwrap_or(&[]),
                None => &[],
            };
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
            self.next?;
        }
    }
}

/// Remove emulation-prevention bytes, producing the RBSP.
///
/// `scratch` is caller-owned and reused: a decoder processes tens of thousands
/// of NAL units, so this must not allocate per call. Cleared, not freed — the
/// same effect `FFmpeg` gets from a persistent `rbsp_buffer`, obtained with
/// ownership instead of a manual free list.
///
/// The rule (H.264 §7.4.1.1): a `03` preceded by two or more zero bytes is an
/// escape and is dropped. Well-formed EBSP never contains three consecutive
/// zeros, so "two or more" and "exactly two" agree on valid input and the
/// former is the more forgiving reading of malformed input.
pub fn to_rbsp<'s>(ebsp: &[u8], scratch: &'s mut Vec<u8>) -> &'s [u8] {
    scratch.clear();
    let mut zeros = 0u32;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        scratch.push(b);
    }
    scratch
}

/// Insert emulation-prevention bytes, producing the EBSP.
///
/// Appends to `out` without clearing it, so a caller can build a whole access
/// unit in one buffer.
pub fn to_ebsp(rbsp: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0u32;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// Whether `buf` contains a three-byte sequence H.264 §7.4.1.1 forbids inside a
/// NAL payload: `00 00 00`, `00 00 01` or `00 00 02`.
///
/// The invariant [`to_ebsp`] establishes, and the reason the escape byte exists.
/// `00 00 03` is deliberately *not* a violation — it is what escaping produces.
#[must_use]
pub fn violates_ebsp_constraint(buf: &[u8]) -> bool {
    buf.windows(3).any(|w| matches!(w, [0, 0, 0..=2]))
}
