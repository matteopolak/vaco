//! De-escaping into a buffer the fast bit reader can use, in one pass.
//!
//! Written from ITU-T H.264 §7.3.1 and §7.4.1.1
//! (`emulation_prevention_three_byte`); ITU-T H.265 §7.3.1.1 and ITU-T H.266
//! §7.3.1.1 define it identically.

use vaco_bitstream::{BitReader, Padded, annexb};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// A reusable buffer holding one NAL unit's RBSP, already padded for
/// [`BitReader`].
///
/// # Why this is not just `Vec<u8>` plus `to_rbsp`
///
/// A parser does three things to every NAL unit, and there are tens of thousands
/// of them in a file:
///
/// 1. strip emulation-prevention bytes (`annexb::to_rbsp` → `&[u8]`),
/// 2. get a [`Padded`] view so the reader's eight-byte refill is legal
///    (`Padded::from_slice_copying` → a **second** copy into a **second**
///    buffer),
/// 3. reuse both buffers next time.
///
/// Steps 1 and 2 are the same copy. `RbspBuf` writes the de-escaped bytes
/// straight into a buffer whose tail already holds [`Padded::PAD`] zeros, so
/// [`RbspBuf::padded`] and [`RbspBuf::reader`] are free, and the allocation is
/// reused across units: cleared, never freed.
///
/// # Budget
///
/// [`RbspBuf::fill`] charges the *growth* of the buffer against the caller's
/// [`Budget`], not its length — a parser that processes ten thousand 4 KiB NAL
/// units through one `RbspBuf` is charged for the high-water mark, once, which
/// is what it actually costs. The declared-size amplification the budget exists
/// to stop cannot happen here anyway: the output of de-escaping is never longer
/// than its input, so the only size that matters is one already in memory.
#[derive(Debug, Default)]
pub struct RbspBuf {
    /// De-escaped bytes followed by `Padded::PAD` zeros.
    buf: Vec<u8>,
    /// Bytes before the padding.
    logical: usize,
    /// Bytes charged to a budget so far, so growth is charged once.
    charged: u64,
    /// Whether the source contained an emulation-prevention byte, which tells a
    /// caller whether byte positions in the RBSP still map to the EBSP.
    escaped: bool,
}

impl RbspBuf {
    /// An empty buffer that has never allocated.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            logical: 0,
            charged: 0,
            escaped: false,
        }
    }

    /// Replace the contents with the RBSP of `ebsp`.
    ///
    /// `ebsp` is a whole NAL unit — header byte included — exactly as it appears
    /// in the stream. The header is *not* stripped: RBSP bit positions are
    /// conventionally counted from the start of the NAL unit (that is what the
    /// specification's syntax tables and every trace tool number from), and a
    /// caller that wants the payload skips the header bits on the reader.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the buffer would have to grow past the
    /// budget's caps.
    pub fn fill(&mut self, ebsp: &[u8], budget: &mut Budget) -> Result<()> {
        let needed = (ebsp.len() as u64).saturating_add(Padded::PAD as u64);
        if needed > self.charged {
            budget.charge(needed - self.charged)?;
            self.charged = needed;
        }

        self.buf.clear();
        self.escaped = false;
        let mut zeros = 0u32;
        for &b in ebsp {
            // §7.4.1.1: within a NAL unit, `00 00 03` is an escape and the `03`
            // is discarded. "Two or more zeros" rather than "exactly two"
            // because well-formed EBSP never contains three consecutive zeros,
            // so the two readings agree on valid input and this one is the more
            // forgiving on malformed input. Same rule as
            // `vaco_bitstream::annexb::to_rbsp`, and `tests/agreement.rs`
            // asserts byte equality with it.
            if zeros >= 2 && b == 3 {
                zeros = 0;
                self.escaped = true;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            self.buf.push(b);
        }

        self.logical = self.buf.len();
        self.buf.resize(self.logical + Padded::PAD, 0);
        Ok(())
    }

    /// The RBSP bytes, padding excluded.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.buf.get(..self.logical).unwrap_or(&[])
    }

    /// A padded view, for [`BitReader::new_padded`].
    ///
    /// Never `None` after [`RbspBuf::fill`]: the padding is established by
    /// construction. It is still an `Option` because [`Padded::new`] verifies
    /// the invariant rather than trusting it, which is what keeps that type
    /// sound without `unsafe`.
    #[must_use]
    pub fn padded(&self) -> Option<Padded<'_>> {
        Padded::new(&self.buf, self.logical)
    }

    /// A reader over the RBSP, on the padded fast path where possible.
    ///
    /// Falls back to [`BitReader::new`] over the logical bytes if the padding
    /// check somehow fails, which is correct and only slightly slower near the
    /// end of the buffer — never incorrect.
    #[must_use]
    pub fn reader(&self) -> BitReader<'_> {
        self.padded()
            .map_or_else(|| BitReader::new(self.as_slice()), BitReader::new_padded)
    }

    /// Number of RBSP bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.logical
    }

    /// Whether the RBSP is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.logical == 0
    }

    /// Whether the source actually contained an emulation-prevention byte.
    ///
    /// False for the overwhelming majority of real NAL units, and a caller that
    /// needs to map an RBSP offset back to a stream offset can take a shortcut
    /// when this is false.
    #[must_use]
    pub const fn was_escaped(&self) -> bool {
        self.escaped
    }

    /// Release the buffer's memory and the budget charge with it.
    ///
    /// A parser calls this on `flush`/seek only if it wants the memory back;
    /// keeping the allocation is the normal case and is why the type exists.
    pub fn release(&mut self, budget: &mut Budget) {
        budget.release(self.charged);
        self.charged = 0;
        self.buf = Vec::new();
        self.logical = 0;
        self.escaped = false;
    }
}

/// Append the EBSP form of `rbsp` — emulation-prevention bytes inserted — to
/// `out`, charging the growth against `budget`.
///
/// Appends rather than clears, so a caller can build a whole access unit in one
/// buffer. Delegates the escaping rule itself to
/// [`vaco_bitstream::annexb::to_ebsp`]; what this adds is the budget, which
/// matters because escaping can grow the input by up to 50% (`00 00 00 00 …`)
/// and that growth is attacker-controlled.
///
/// # Errors
///
/// [`Error::LimitExceeded`] if the worst-case escaped size would not fit the
/// budget. The check is against the worst case rather than the actual size,
/// because refusing *after* the allocation defeats the purpose.
pub fn escape_into(rbsp: &[u8], out: &mut Vec<u8>, budget: &mut Budget) -> Result<()> {
    // Worst case: every byte after the first two triggers an escape, giving
    // ceil(3n/2). Computed in u64 so a huge input cannot wrap.
    let worst = (rbsp.len() as u64)
        .checked_mul(3)
        .map(|n| n.div_ceil(2))
        .ok_or(Error::LimitExceeded {
            limit: "ebsp_escape",
            requested: u64::MAX,
            cap: budget.limits().max_alloc_single,
        })?;
    budget.charge(worst)?;
    annexb::to_ebsp(rbsp, out);
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

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    #[test]
    fn removes_the_escape_byte() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[0x67, 0x00, 0x00, 0x03, 0x01, 0xFF], &mut b)
            .unwrap();
        assert_eq!(r.as_slice(), &[0x67, 0x00, 0x00, 0x01, 0xFF]);
        assert!(r.was_escaped());
    }

    #[test]
    fn leaves_unescaped_data_alone() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[0x67, 0x42, 0xC0, 0x1E], &mut b).unwrap();
        assert_eq!(r.as_slice(), &[0x67, 0x42, 0xC0, 0x1E]);
        assert!(!r.was_escaped());
    }

    #[test]
    fn a_three_after_a_single_zero_is_data() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[0x00, 0x03, 0x00, 0x00, 0x03], &mut b).unwrap();
        // The first 03 follows one zero and survives; the second follows two
        // and is dropped.
        assert_eq!(r.as_slice(), &[0x00, 0x03, 0x00, 0x00]);
    }

    #[test]
    fn the_padded_view_exists_and_is_zero() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[1, 2, 3], &mut b).unwrap();
        let p = r.padded().expect("fill establishes the padding");
        assert_eq!(p.logical_len(), 3);
        assert_eq!(p.as_bytes().len(), 3 + Padded::PAD);
        assert!(p.as_bytes()[3..].iter().all(|&x| x == 0));
    }

    #[test]
    fn refilling_smaller_shrinks_the_logical_length() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[1, 2, 3, 4, 5], &mut b).unwrap();
        r.fill(&[9], &mut b).unwrap();
        assert_eq!(r.as_slice(), &[9]);
        assert_eq!(r.len(), 1);
        assert!(
            r.padded().expect("still padded").as_bytes()[1..]
                .iter()
                .all(|&x| x == 0)
        );
    }

    #[test]
    fn growth_is_charged_once_not_per_fill() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[0; 1000], &mut b).unwrap();
        let after_first = b.committed();
        for _ in 0..100 {
            r.fill(&[0; 1000], &mut b).unwrap();
        }
        assert_eq!(b.committed(), after_first);
    }

    #[test]
    fn a_tiny_budget_refuses_rather_than_allocating() {
        let mut b = Budget::new(Limits::tiny());
        let mut r = RbspBuf::new();
        let big = vec![0u8; 1 << 20];
        let err = r.fill(&big, &mut b).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn empty_input_is_an_empty_rbsp() {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&[], &mut b).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.as_slice(), b"");
        assert!(r.padded().is_some());
    }

    #[test]
    fn escaping_is_budgeted_and_inverts() {
        let mut b = budget();
        let src = [0u8, 0, 0, 0, 1, 2];
        let mut out = Vec::new();
        escape_into(&src, &mut out, &mut b).unwrap();
        assert!(!vaco_bitstream::annexb::violates_ebsp_constraint(&out));
        let mut r = RbspBuf::new();
        r.fill(&out, &mut b).unwrap();
        assert_eq!(r.as_slice(), &src);
    }
}
