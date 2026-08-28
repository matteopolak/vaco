//! ATSC A/53 closed-caption extraction from MPEG-2 picture `user_data()`.
//!
//! # A different mechanism reaching the same payload
//!
//! H.264 and HEVC carry A/53 captions in an SEI message, behind a T.35
//! country and provider code. MPEG-2 has no SEI: captions ride directly in a
//! `user_data_start_code` (`0x000001B2`) element, and the T.35 prefix is
//! absent entirely — the payload begins at `user_data_identifier`. Everything
//! from that identifier onward is identical to the SEI carriage, which is why
//! the two agree on the bytes this module hands back.
//!
//! ```text
//! user_data() {
//!     user_data_start_code    32   0x000001B2
//!     user_data_identifier    32   0x47413934 'GA94'
//!     user_data_type_code      8   0x03  -> MPEG_cc_data()
//!     cc_data()                     ANSI/CTA-708 Table 2
//!     marker_bits              8   '1111 1111'
//! }
//! ```
//!
//! Structure and constants from ATSC A/53 Part 4:2009 §6.2.2 (Table 6.6),
//! §6.2.3 (Tables 6.7 and 6.9) and §6.2.3.1 (Table 6.10), read from the
//! standard rather than recalled.
//!
//! # Why no emulation-prevention unescaping happens here
//!
//! An H.264/HEVC SEI payload has already had its `emulation_prevention_three_
//! byte`s removed by the time a caller reaches the equivalent module in those
//! crates. MPEG-2 has no such escape: ITU-T H.262 §6.2.2.2.2 instead
//! *forbids* user data from containing a string of 23 or more consecutive
//! zero bits, so a start code cannot occur inside one and the payload is
//! already literal. Scanning to the next start code is therefore exact, not
//! approximate.
//!
//! # Consume captions in presentation order — getting this wrong fails silently
//!
//! This is the one thing a caller must get right. CEA-608 is a *sequential*
//! byte stream carrying a stateful command language: a control code in one
//! picture sets the mode that the character pairs in later pictures land in.
//! Pictures reach a parser in **decode** order, which with B-frames is not
//! presentation order — so concatenating the payloads in the order they are
//! parsed interleaves the caption stream and destroys it.
//!
//! Measured, on a real broadcast capture: the same 361 payloads decoded in
//! decode order produce `"    s  itesciti. now"`, and in presentation order
//! produce `" its cities now."`. **Both decode with zero parity errors** —
//! nothing in the caption layer signals the mistake, because every byte pair
//! is individually valid and only their sequence is wrong.
//!
//! So attach what this module returns to *its own picture* and let the
//! reordering that already happens between decode and output carry it, which
//! is what `FrameSideData::ClosedCaptions` on a `Frame` does by construction.
//! Do not accumulate payloads into a buffer as they are parsed.
//!
//! # Allocation
//!
//! None. [`iter_cc_data`] is a lazy iterator yielding subslices of the
//! caller's buffer, and `cc_count` is five bits wide so no single element can
//! select more than 93 bytes.

use vaco_bitstream::annexb;

/// `user_data_start_code`'s fourth byte, ITU-T H.262 Table 6-1.
const USER_DATA_START: u8 = 0xB2;

/// `user_identifier` `GA94`, ATSC A/53 Part 4 Table 6.7.
pub const USER_IDENTIFIER_GA94: u32 = 0x4741_3934;

/// `user_data_type_code` for `MPEG_cc_data()`, ATSC A/53 Part 4 Table 6.9.
pub const USER_DATA_TYPE_CC: u8 = 0x03;

/// `cc_count` is a 5-bit field, so one `cc_data()` carries at most 31
/// three-byte triplets.
pub const MAX_CC_DATA_BYTES: usize = 31 * 3;

/// Iterate every A/53 caption `user_data()` element in `data`, yielding each
/// one's `cc_data` triplet bytes.
///
/// A buffer may legitimately hold several pictures, and A/53 §6.2.1 permits
/// more than one `user_data()` after a given picture header (only repeating
/// the same `user_data_type_code` is forbidden), so this yields all of them
/// in stream order rather than assuming one.
///
/// Non-caption user data — `DTG1` active-format description, bar data, a
/// vendor's private blob — is skipped silently. A stream is expected to carry
/// those, so they are not failures.
pub fn iter_cc_data(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    UserDataIter { data, pos: 0 }.filter_map(cc_data_after_identifier)
}

/// The first A/53 caption payload in `data`, if any.
///
/// Convenience over [`iter_cc_data`] for the common single-picture case.
#[must_use]
pub fn find_cc_data(data: &[u8]) -> Option<&[u8]> {
    iter_cc_data(data).next()
}

/// Yields the payload of every `user_data_start_code` element in a buffer.
struct UserDataIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for UserDataIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            let at = annexb::find_start_code(self.data, self.pos)?;
            // `find_start_code` points at the `00 00 01` prefix; the start
            // code value is the byte after it and the payload begins after
            // that.
            let code = *self.data.get(at.saturating_add(3))?;
            let payload_start = at.saturating_add(4);
            self.pos = payload_start;
            if code != USER_DATA_START {
                continue;
            }
            // The element runs to the next start code, or to the end of the
            // buffer when this is the last one.
            let end = annexb::find_start_code(self.data, payload_start).unwrap_or(self.data.len());
            let payload = self.data.get(payload_start..end)?;
            self.pos = end;
            return Some(payload);
        }
    }
}

/// Parse `user_data_identifier`, `user_data_type_code` and the `cc_data()`
/// that follows, from one `user_data()` element's payload.
#[must_use]
pub fn cc_data_after_identifier(data: &[u8]) -> Option<&[u8]> {
    let identifier = u32::from_be_bytes(*data.first_chunk::<4>()?);
    if identifier != USER_IDENTIFIER_GA94 {
        return None;
    }
    if *data.get(4)? != USER_DATA_TYPE_CC {
        return None;
    }
    cc_data_triplets(data.get(5..)?)
}

/// Take the `cc_count * 3` triplet bytes out of a `cc_data()` structure.
///
/// `None` when `process_cc_data_flag` is clear — CEA-708 defines that flag as
/// "present but not to be processed", so honouring it is the difference
/// between reproducing the transmitted caption and inventing one the
/// broadcaster suppressed — or when the declared `cc_count` runs past the
/// bytes present, which is truncation rather than a shorter message.
#[must_use]
pub fn cc_data_triplets(cc_data: &[u8]) -> Option<&[u8]> {
    let header = *cc_data.first()?;
    if header & 0x40 == 0 {
        return None;
    }
    let cc_count = usize::from(header & 0x1F);
    // Byte 1 is `em_data`, which carries no caption content.
    cc_data.get(2..2 + cc_count * 3)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// A `cc_data()` carrying `n` triplets whose first byte counts up, so a
    /// test can tell one element's payload from another's.
    fn cc_data_element(n: u8, tag: u8) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x01, USER_DATA_START];
        v.extend_from_slice(&USER_IDENTIFIER_GA94.to_be_bytes());
        v.push(USER_DATA_TYPE_CC);
        v.push(0xC0 | n); // reserved + process_cc_data_flag, cc_count = n
        v.push(0xFF); // em_data
        for i in 0..n {
            v.extend_from_slice(&[0xFC, tag, i]);
        }
        v.push(0xFF); // trailing marker_bits
        v
    }

    #[test]
    fn extracts_one_element() {
        let buf = cc_data_element(2, 0xAA);
        let cc = find_cc_data(&buf).expect("one caption element");
        assert_eq!(cc, &[0xFC, 0xAA, 0, 0xFC, 0xAA, 1]);
    }

    #[test]
    fn extracts_every_element_in_stream_order() {
        let mut buf = cc_data_element(1, 0x11);
        buf.extend_from_slice(&cc_data_element(2, 0x22));
        // A picture start code between them must not confuse the scan.
        buf.extend_from_slice(&[0x00, 0x00, 0x01, 0x00, 0xDE, 0xAD]);
        buf.extend_from_slice(&cc_data_element(1, 0x33));
        let tags: Vec<u8> = iter_cc_data(&buf).map(|cc| cc[1]).collect();
        assert_eq!(tags, [0x11, 0x22, 0x33]);
    }

    #[test]
    fn skips_non_caption_user_data() {
        // A DTG1 (active format description) element, then a real one.
        let mut buf = vec![0x00, 0x00, 0x01, USER_DATA_START];
        buf.extend_from_slice(b"DTG1");
        buf.extend_from_slice(&[0x41, 0x00]);
        buf.extend_from_slice(&cc_data_element(1, 0x55));
        let found: Vec<&[u8]> = iter_cc_data(&buf).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0][1], 0x55);
    }

    #[test]
    fn skips_bar_data_type_code() {
        let mut buf = vec![0x00, 0x00, 0x01, USER_DATA_START];
        buf.extend_from_slice(&USER_IDENTIFIER_GA94.to_be_bytes());
        buf.push(0x06); // bar_data, not captions
        buf.extend_from_slice(&[0xC1, 0xFF, 0x00, 0x00, 0x00]);
        assert_eq!(find_cc_data(&buf), None);
    }

    #[test]
    fn honours_process_cc_data_flag() {
        let mut buf = cc_data_element(2, 0xAA);
        buf[9] &= !0x40; // the cc_data header byte
        assert_eq!(find_cc_data(&buf), None);
    }

    #[test]
    fn truncated_cc_count_is_rejected_not_clamped() {
        let mut buf = cc_data_element(2, 0xAA);
        buf[9] = 0xC0 | 31; // claim 31 triplets, supply 2
        assert_eq!(find_cc_data(&buf), None);
    }

    #[test]
    fn never_exceeds_the_five_bit_count_bound() {
        for n in 0..=31u8 {
            let buf = cc_data_element(n, 0x7F);
            let cc = find_cc_data(&buf).expect("well-formed element");
            assert_eq!(cc.len(), usize::from(n) * 3);
            assert!(cc.len() <= MAX_CC_DATA_BYTES);
        }
    }

    #[test]
    fn empty_and_malformed_inputs_do_not_panic() {
        assert_eq!(find_cc_data(&[]), None);
        assert_eq!(find_cc_data(&[0x00, 0x00, 0x01]), None);
        assert_eq!(find_cc_data(&[0x00, 0x00, 0x01, USER_DATA_START]), None);
        assert_eq!(cc_data_triplets(&[]), None);
        assert_eq!(cc_data_triplets(&[0xC0]), None);
    }
}
