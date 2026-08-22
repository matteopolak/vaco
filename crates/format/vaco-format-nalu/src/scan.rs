//! Resumable Annex-B start-code scanning, for parsers fed in chunks.
//!
//! Written from ITU-T H.264 Annex B §B.1.1: a `start_code_prefix_one_3bytes` of
//! `00 00 01`, optionally preceded by a `zero_byte`.

use vaco_bitstream::annexb;

/// One start code located in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartCode {
    /// Index of the start code's first byte, **including** the Annex B
    /// `zero_byte` when one is present. `offset + len` is where the NAL unit's
    /// bytes begin.
    pub offset: usize,
    /// 3 or 4.
    pub len: u8,
}

impl StartCode {
    /// Index of the first byte of the NAL unit that follows.
    #[must_use]
    pub const fn payload_offset(&self) -> usize {
        self.offset.saturating_add(self.len as usize)
    }
}

/// A start-code scanner that remembers what it has already ruled out.
///
/// # The problem it solves
///
/// A [`Parser`](vaco_codec_core::Parser) is handed a buffer that *grows*: the
/// driver appends the next chunk and calls again with the same unconsumed
/// prefix. Scanning from the beginning every time is quadratic in the number of
/// chunks, and on a stream that never produces a complete unit — which is
/// exactly the shape a fuzzer finds — that is a hang rather than a slowdown.
///
/// `Scanner` records how far it has looked. A re-presented prefix is not
/// re-examined, so the total work stays linear in the bytes seen however the
/// stream is chopped up.
///
/// # The two-byte tail
///
/// After an unsuccessful search over `buf`, only `buf.len() - 2` bytes are
/// genuinely ruled out: the final two could be the `00 00` of a start code
/// whose `01` has not arrived. Getting this wrong is the classic chunked-parser
/// bug — the unit boundary lands exactly on a chunk boundary and vanishes.
///
/// # Example
///
/// ```
/// use vaco_format_nalu::Scanner;
///
/// // The start code straddles two chunks.
/// let mut s = Scanner::new();
/// let mut buf = vec![0xAA, 0x00, 0x00];
/// assert_eq!(s.find(&buf, 0), None);
/// buf.push(0x01);
/// let sc = s.find(&buf, 0).expect("now complete");
/// assert_eq!((sc.offset, sc.len), (1, 3));
/// assert_eq!(sc.payload_offset(), 4);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct Scanner {
    /// Bytes at the front of the caller's buffer already known to contain no
    /// start code that begins at or before this index.
    scanned: usize,
}

impl Scanner {
    /// A scanner that has ruled nothing out.
    #[must_use]
    pub const fn new() -> Self {
        Self { scanned: 0 }
    }

    /// The next start code at or after `from`, or `None` if the buffer does not
    /// contain one yet.
    ///
    /// `from` is a floor, not a seek: the scan actually begins at the later of
    /// `from` and the point this scanner has already reached, so calling with
    /// `from = 0` on a re-presented buffer costs nothing.
    ///
    /// The scan itself is [`vaco_bitstream::annexb::find_start_code`] — the
    /// project's single definition of where a start code is. This adds only the
    /// resumption and the `zero_byte` classification.
    pub fn find(&mut self, buf: &[u8], from: usize) -> Option<StartCode> {
        let begin = from.max(self.scanned);
        let Some(i) = annexb::find_start_code(buf, begin) else {
            // Everything but the final two bytes is ruled out; those two could
            // still become `00 00 01`. If the buffer was shorter than `begin`
            // the search examined nothing, so the watermark must not move —
            // advancing it there would skip bytes that a later, longer buffer
            // needs looked at.
            let limit = buf.len().saturating_sub(2);
            if begin <= limit {
                self.scanned = limit;
            }
            return None;
        };
        // Do not advance past a code we are returning: a caller may
        // legitimately ask for the same one twice before consuming it.
        self.scanned = i;
        let four = i >= 1 && buf.get(i - 1) == Some(&0);
        Some(StartCode {
            offset: if four { i - 1 } else { i },
            len: if four { 4 } else { 3 },
        })
    }

    /// Tell the scanner that `n` bytes have been removed from the front of the
    /// buffer it is scanning.
    pub const fn consume(&mut self, n: usize) {
        self.scanned = self.scanned.saturating_sub(n);
    }

    /// How far into the buffer the scanner has looked.
    #[must_use]
    pub const fn scanned(&self) -> usize {
        self.scanned
    }

    /// Forget everything, after a seek or a discontinuity.
    pub const fn reset(&mut self) {
        self.scanned = 0;
    }
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

    #[test]
    fn three_and_four_byte_codes_are_distinguished() {
        let mut s = Scanner::new();
        let buf = [0xAA, 0x00, 0x00, 0x01, 0x67];
        assert_eq!(s.find(&buf, 0), Some(StartCode { offset: 1, len: 3 }));
        let mut s = Scanner::new();
        let buf = [0xAA, 0x00, 0x00, 0x00, 0x01, 0x67];
        assert_eq!(s.find(&buf, 0), Some(StartCode { offset: 1, len: 4 }));
    }

    #[test]
    fn a_code_at_offset_zero_is_three_bytes() {
        let mut s = Scanner::new();
        let buf = [0x00, 0x00, 0x01, 0x67];
        assert_eq!(s.find(&buf, 0), Some(StartCode { offset: 0, len: 3 }));
    }

    #[test]
    fn a_boundary_split_across_chunks_is_still_found() {
        for split in 0..6 {
            let whole = [0xAA, 0xBB, 0x00, 0x00, 0x01, 0x67];
            let mut s = Scanner::new();
            let first = &whole[..split];
            let _ = s.find(first, 0);
            let found = s.find(&whole, 0);
            assert_eq!(
                found,
                Some(StartCode { offset: 2, len: 3 }),
                "lost the boundary when the first chunk was {split} bytes"
            );
        }
    }

    #[test]
    fn asking_twice_returns_the_same_code() {
        let mut s = Scanner::new();
        let buf = [0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x01, 0x68];
        let a = s.find(&buf, 0);
        let b = s.find(&buf, 0);
        assert_eq!(a, b);
    }

    #[test]
    fn from_advances_past_a_consumed_code() {
        let mut s = Scanner::new();
        let buf = [0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x01, 0x68];
        let a = s.find(&buf, 0).unwrap();
        let b = s.find(&buf, a.payload_offset()).unwrap();
        assert_eq!(b.offset, 4);
    }

    #[test]
    fn consume_shifts_the_watermark() {
        let mut s = Scanner::new();
        let buf = vec![0xFFu8; 64];
        assert_eq!(s.find(&buf, 0), None);
        assert_eq!(s.scanned(), 62);
        s.consume(60);
        assert_eq!(s.scanned(), 2);
        s.consume(1000);
        assert_eq!(s.scanned(), 0);
    }

    #[test]
    fn short_buffers_do_not_underflow() {
        let mut s = Scanner::new();
        for n in 0..3 {
            let buf = vec![0u8; n];
            assert_eq!(s.find(&buf, 0), None);
            assert!(s.scanned() <= n);
        }
    }

    #[test]
    fn resumption_never_skips_a_code_the_full_scan_would_find() {
        // Growing a buffer one byte at a time must find every code exactly
        // where a from-scratch scan would.
        let whole: Vec<u8> = (0..40u8)
            .map(|i| match i % 7 {
                0 | 1 => 0,
                2 => 1,
                _ => i,
            })
            .collect();
        let mut s = Scanner::new();
        let mut from = 0usize;
        let mut incremental = Vec::new();
        for n in 0..=whole.len() {
            while let Some(sc) = s.find(&whole[..n], from) {
                if sc.payload_offset() > n {
                    break;
                }
                incremental.push(sc.offset);
                from = sc.payload_offset();
            }
        }
        let mut reference = Vec::new();
        let mut i = 0usize;
        while let Some(sc) = annexb::find_start_code(&whole, i) {
            let four = sc >= 1 && whole[sc - 1] == 0;
            reference.push(if four { sc - 1 } else { sc });
            i = sc + 3;
        }
        assert_eq!(incremental, reference);
    }
}
