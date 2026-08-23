//! The Annex-B/MPEG start-code convention, shared by several bitstream
//! families in [`crate::bitstream`].
//!
//! H.264 (ISO/IEC 14496-10 Annex B), HEVC (23008-2 Annex B), VVC (23090-3
//! Annex B), MPEG-1/2 video (11172-2/13818-2), MPEG-4 Part 2 (14496-2), VC-1
//! (SMPTE 421M Annex E) and the Chinese AVS family (GB/T 20090, GY/T
//! 299.1/AVS2, GY/T 358/AVS3) all delimit units with the three-byte sequence
//! `00 00 01`, optionally preceded by an extra zero byte. This is a
//! format-dictated convention shared by public specifications, not `FFmpeg`'s
//! expression of it (D7).
//!
//! # What this module does *not* do
//!
//! It finds start-code boundaries; it does not know NAL types, does not
//! group a parameter set with the picture it belongs to, and does not
//! detect keyframes. Splitting at every start code produces one packet per
//! NAL/start-code unit rather than the reference's per-access-unit grouping.
//! For H.264 and HEVC, [`crate::bitstream`] prefers a real
//! [`vaco_codec_core::Parser`] when the caller's `ParserProvider` has one,
//! which does group correctly; this scanner is the fallback for those two
//! and the only implementation for the rest. See the crate docs.

/// Byte offsets of every `00 00 01` start code in `data`, in ascending order.
///
/// Overlapping matches are not possible for a 3-byte needle scanned
/// byte-by-byte, so this is a plain forward scan.
#[must_use]
pub(crate) fn start_codes(data: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    if data.len() < 3 {
        return out;
    }
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data.get(i..i + 3) == Some(&[0, 0, 1]) {
            out.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// Split `data` into spans `(start, end)`, each running from one start code
/// up to (but excluding) the next, or to `data.len()` for the last one.
///
/// A leading span before the first start code is dropped — real encoder
/// output never has one, and a hostile file that does would otherwise
/// contribute an un-delimited "packet" with no sync of its own.
#[must_use]
pub(crate) fn spans(data: &[u8]) -> Vec<(usize, usize)> {
    let marks = start_codes(data);
    let mut out = Vec::new();
    for (i, &start) in marks.iter().enumerate() {
        let end = marks.get(i + 1).copied().unwrap_or(data.len());
        out.push((start, end));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_start_code() {
        let data = [0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0, 0, 0, 1, 0x65];
        // The last unit is written with the common 4-byte lead-in `00 00 00
        // 01`; the 3-byte needle within it is found at offset 10, not 9.
        assert_eq!(start_codes(&data), vec![0, 5, 10]);
    }

    #[test]
    fn spans_cover_the_whole_buffer_after_the_first_start_code() {
        let data = [0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68];
        let s = spans(&data);
        assert_eq!(s, vec![(0, 5), (5, 9)]);
    }

    #[test]
    fn no_start_code_yields_no_spans() {
        assert!(spans(&[1, 2, 3, 4]).is_empty());
        assert!(spans(&[]).is_empty());
    }
}
