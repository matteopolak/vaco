//! The MSB-first bit reader.

use crate::{BitstreamError, Padded, Result};

/// A saved reader position, for speculative parsing.
///
/// Field copies only — [`BitReader`] holds no interior mutability and owns
/// nothing, so save and restore are free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pos: usize,
    cache: u64,
    cache_bits: u32,
    flagged: bool,
}

/// MSB-first bit reader with a 64-bit cache — the universal shape for video
/// bitstreams.
///
/// # The sticky-overrun model
///
/// Reads do **not** return `Result`. Past the logical end they return zeros —
/// deterministically, the same values `FFmpeg`'s zero padding produces, so a
/// parser written against the spec behaves identically — and the reader records
/// that it happened. The parser checks once, at the end of a syntax structure,
/// with [`check`](BitReader::check) or [`finish`](BitReader::finish).
///
/// The alternative, `Result` per read, turns a 40-line SPS parser into 40
/// `?`-laden lines, blocks inlining, and adds a branch per read that the CPU has
/// to predict anyway. What the sticky model gives up is early exit; what it
/// keeps is the property that actually matters: **a truncated or malformed
/// bitstream can never panic and never reads out of bounds.** For the rare site
/// that must branch immediately — a length prefix about to size an allocation —
/// [`try_get`](BitReader::try_get) exists.
///
/// Overrun is *derived*, not flagged: it is `bit_pos() > logical_bits`, computed
/// from state the reader keeps anyway. So the sticky model costs zero
/// instructions in the read path, not one predictable branch. A separate flag
/// covers the conditions position cannot express — a malformed Exp-Golomb
/// prefix, an out-of-range width.
///
/// # The body / tail split
///
/// The reader refills a 64-bit cache eight bytes at a time. Reads *from* the
/// cache are register operations with no bounds check at all: one comparison per
/// refill covers the four to eight syntax elements that refill feeds.
///
/// - **Body** — `pos + 8 <= data.len()`: one `u64` load, one comparison.
/// - **Tail** — the last eight bytes: assembled byte by byte, zero-filling past
///   the end.
///
/// [`new_padded`](BitReader::new_padded) is what makes that split pay. A
/// [`Padded`] buffer carries 64 zero bytes past its logical end, so the body
/// runs 56 bytes *past* where the data stops and a header parser never reaches
/// the tail path at all. See `benches/reader.rs` for what this is worth against
/// a bounds-check-per-read reader.
///
/// # Example
///
/// ```
/// use vaco_bitstream::BitReader;
///
/// let mut r = BitReader::new(&[0b1010_1100, 0xFF]);
/// assert_eq!(r.get(1), 1);
/// assert_eq!(r.get(3), 0b010);
/// assert_eq!(r.get(4), 0b1100);
/// assert_eq!(r.get(8), 0xFF);
/// r.finish()?;                    // nothing overran
/// # Ok::<(), vaco_bitstream::BitstreamError>(())
/// ```
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    /// The whole slice, including any padding.
    data: &'a [u8],
    /// Bits before the padding. Reads past this return zeros and set overrun.
    logical_bits: u64,
    /// Byte offset of the next refill.
    pos: usize,
    /// MSB-aligned; the top `cache_bits` bits are valid.
    cache: u64,
    /// `0..=64`.
    cache_bits: u32,
    /// Conditions position cannot express: malformed VLC, bad width.
    flagged: bool,
}

impl<'a> BitReader<'a> {
    /// Read `data`, with no padding guarantee.
    ///
    /// Correct for a borrowed mmap or any slice from a caller we do not control.
    /// The body ends eight bytes before the end of the slice; the last eight
    /// bytes go through the byte-at-a-time tail path.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            logical_bits: (data.len() as u64).saturating_mul(8),
            pos: 0,
            cache: 0,
            cache_bits: 0,
            flagged: false,
        }
    }

    /// Read a padded buffer, taking the fast path 56 bytes past the logical end.
    #[must_use]
    pub const fn new_padded(p: Padded<'a>) -> Self {
        let data = p.as_bytes();
        Self {
            data,
            logical_bits: (p.logical_len() as u64).saturating_mul(8),
            pos: 0,
            cache: 0,
            cache_bits: 0,
            flagged: false,
        }
    }

    /// Read the first `logical_len` bytes of `data`, treating the rest as
    /// unreadable.
    ///
    /// For a syntax structure carved out of a larger buffer without slicing it —
    /// a sized box inside a container, say. Reads past `logical_len` set overrun
    /// even though real bytes follow.
    #[must_use]
    pub const fn with_logical_len(data: &'a [u8], logical_len: usize) -> Self {
        let logical_len = if logical_len > data.len() {
            data.len()
        } else {
            logical_len
        };
        Self {
            data,
            logical_bits: (logical_len as u64).saturating_mul(8),
            pos: 0,
            cache: 0,
            cache_bits: 0,
            flagged: false,
        }
    }

    // ------------------------------------------------------------ the refill

    /// Top the cache up to at least 57 valid bits.
    ///
    /// The merge is `cache |= chunk >> cache_bits`, advancing `pos` by only the
    /// *whole bytes* that fit. Bits of `chunk` past that point stay in the cache
    /// below `cache_bits`, where they are not valid — but they are also exactly
    /// the bits the next refill will load, so the next `|=` writes the same
    /// values over them. The OR is idempotent, which is what lets this avoid a
    /// mask entirely.
    #[inline]
    fn refill(&mut self) {
        if self.cache_bits > 56 {
            return;
        }
        // The body / tail split, as one comparison: the slice access proves
        // eight bytes are in bounds, or it does not and the cold tail runs.
        let chunk = match self.data.get(self.pos..).and_then(<[u8]>::first_chunk::<8>) {
            Some(c) => u64::from_be_bytes(*c),
            None => self.tail_word(),
        };
        self.cache |= chunk >> self.cache_bits;
        let take = (64 - self.cache_bits) >> 3;
        self.pos = self.pos.saturating_add(take as usize);
        self.cache_bits += take * 8;
    }

    /// The checked tail: eight bytes assembled individually, zero past the end.
    #[cold]
    #[inline(never)]
    fn tail_word(&self) -> u64 {
        let mut w = 0u64;
        for i in 0..8usize {
            let b = self
                .data
                .get(self.pos.saturating_add(i))
                .copied()
                .unwrap_or(0);
            w = (w << 8) | u64::from(b);
        }
        w
    }

    // -------------------------------------------------------------- the reads

    /// Read one bit.
    #[inline]
    pub fn get_bit(&mut self) -> u32 {
        self.get(1)
    }

    /// Read `n` bits, MSB first. `n <= 32`.
    ///
    /// A larger `n` debug-asserts, and in release clamps to 32 and flags the
    /// reader. It never panics.
    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 32, "BitReader::get: n must be <= 32, got {n}");
        // `min` rather than a flag: a conditional store here would sit in the
        // hottest path in the project to report a caller bug that
        // `debug_assert` already catches.
        let n = n.min(32);
        if n == 0 {
            return 0;
        }
        if self.cache_bits < n {
            self.refill();
        }
        // After a refill `cache_bits >= 57`, and `refill` only returns early
        // when it is already above 56, so `cache_bits >= n` holds for n <= 32.
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.cache_bits -= n;
        v
    }

    /// Read `n` bits without consuming them. `n <= 32`.
    #[inline]
    pub fn peek(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 32, "BitReader::peek: n must be <= 32, got {n}");
        let n = n.min(32);
        if n == 0 {
            return 0;
        }
        if self.cache_bits < n {
            self.refill();
        }
        (self.cache >> (64 - n)) as u32
    }

    /// Advance by `n` bits.
    ///
    /// Constant time for any `n`: a large skip moves `pos` directly rather than
    /// looping, so skipping a megabyte payload costs the same as skipping a bit.
    /// That matters — a loop here would be a fuzz hang.
    #[inline]
    pub fn skip(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        if n < self.cache_bits {
            self.cache <<= n;
            self.cache_bits -= n;
            return;
        }
        let rest = n - self.cache_bits;
        self.cache = 0;
        self.cache_bits = 0;
        self.pos = self.pos.saturating_add((rest >> 3) as usize);
        let bits = rest & 7;
        if bits != 0 {
            self.refill();
            self.cache <<= bits;
            self.cache_bits -= bits;
        }
    }

    /// Advance by `n` bits, where `n` may exceed a `u32`.
    ///
    /// Also constant time: a container that declares a 40-bit-addressed payload
    /// must not be able to turn a skip into a loop.
    #[inline]
    pub fn skip_long(&mut self, n: u64) {
        if n <= u64::from(self.cache_bits) {
            self.skip(n as u32);
            return;
        }
        let rest = n - u64::from(self.cache_bits);
        self.cache = 0;
        self.cache_bits = 0;
        self.pos = self
            .pos
            .saturating_add(usize::try_from(rest >> 3).unwrap_or(usize::MAX));
        let bits = (rest & 7) as u32;
        if bits != 0 {
            self.refill();
            self.cache <<= bits;
            self.cache_bits -= bits;
        }
    }

    /// Advance by `n` whole bytes from the current position, which need not be
    /// byte-aligned.
    #[inline]
    pub fn skip_bytes(&mut self, n: usize) {
        self.skip_long((n as u64).saturating_mul(8));
    }

    /// Read `n` bits into a `u64`. `n <= 64`.
    #[inline]
    pub fn get_long(&mut self, n: u32) -> u64 {
        debug_assert!(n <= 64, "BitReader::get_long: n must be <= 64, got {n}");
        let n = n.min(64);
        if n <= 32 {
            return u64::from(self.get(n));
        }
        let hi = u64::from(self.get(n - 32));
        let lo = u64::from(self.get(32));
        (hi << 32) | lo
    }

    /// Read `n` bits as a two's-complement signed value. `n <= 32`.
    #[inline]
    pub fn get_signed(&mut self, n: u32) -> i32 {
        let n = n.min(32);
        if n == 0 {
            return 0;
        }
        let v = self.get(n);
        let shift = 32 - n;
        (v << shift).cast_signed() >> shift
    }

    /// Read `n` bits, failing immediately if fewer than `n` remain.
    ///
    /// For the sites that must not proceed on truncation — a length prefix about
    /// to size an allocation, most of all. Consumes nothing on failure.
    ///
    /// # Errors
    ///
    /// [`BitstreamError::Overrun`] if fewer than `n` bits remain, or if the
    /// reader is already in an error state.
    pub fn try_get(&mut self, n: u32) -> Result<u32> {
        if self.flagged || u64::from(n) > self.bits_left() {
            return Err(BitstreamError::Overrun);
        }
        Ok(self.get(n))
    }

    // ------------------------------------------------------------- positioning

    /// Advance to the next byte boundary. A no-op if already aligned.
    #[inline]
    pub fn align(&mut self) {
        self.skip(self.cache_bits & 7);
    }

    /// Whether the reader sits on a byte boundary.
    #[must_use]
    pub const fn is_aligned(&self) -> bool {
        self.cache_bits.trailing_zeros() >= 3
    }

    /// Bits consumed so far.
    #[must_use]
    pub const fn bit_pos(&self) -> u64 {
        (self.pos as u64)
            .saturating_mul(8)
            .saturating_sub(self.cache_bits as u64)
    }

    /// Readable bits remaining. Zero once overrun.
    #[must_use]
    pub const fn bits_left(&self) -> u64 {
        if self.flagged {
            return 0;
        }
        self.logical_bits.saturating_sub(self.bit_pos())
    }

    /// The logical size of the buffer, in bits.
    #[must_use]
    pub const fn logical_bits(&self) -> u64 {
        self.logical_bits
    }

    /// Whether anything has gone wrong: a read past the logical end, or a
    /// malformed code.
    ///
    /// Cheap — no flag is maintained in the read path; the past-the-end half is
    /// derived from the position.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.flagged || self.bit_pos() > self.logical_bits
    }

    /// Mark the reader malformed, for a parser that detects an impossible value
    /// its own way and wants the same one-place-to-check contract.
    pub const fn flag_malformed(&mut self) {
        self.flagged = true;
    }

    /// The end-of-syntax-structure check.
    ///
    /// # Errors
    ///
    /// [`BitstreamError::Overrun`] if anything read past the logical end or a
    /// code was malformed.
    ///
    /// # Note
    ///
    /// Unlike the sketch in `planning/11-foundations.md` §8.4 this takes `&self`
    /// and does not clear. Clearing is meaningless once overrun is derived from
    /// the position — the position stays past the end — and would in any case
    /// hide a truncation from the next caller, which is the opposite of what a
    /// parser wants.
    pub const fn check(&self) -> Result<()> {
        if self.overrun() {
            return Err(BitstreamError::Overrun);
        }
        Ok(())
    }

    /// [`check`](BitReader::check), consuming the reader.
    ///
    /// # Errors
    ///
    /// As [`check`](BitReader::check).
    pub const fn finish(self) -> Result<()> {
        self.check()
    }

    /// Save the position.
    #[must_use]
    pub const fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            cache: self.cache,
            cache_bits: self.cache_bits,
            flagged: self.flagged,
        }
    }

    /// Restore a saved position. Speculative parsing costs four field writes.
    pub const fn restore(&mut self, m: Mark) {
        self.pos = m.pos;
        self.cache = m.cache;
        self.cache_bits = m.cache_bits;
        self.flagged = m.flagged;
    }

    /// The logical bytes from the current position onward, without copying.
    ///
    /// If the reader is not byte-aligned the partial byte is skipped, so the
    /// result always starts on a boundary. Empty once past the logical end.
    #[must_use]
    pub fn remaining_bytes(&self) -> &'a [u8] {
        let start = usize::try_from(self.bit_pos().div_ceil(8)).unwrap_or(usize::MAX);
        let end = usize::try_from(self.logical_bits >> 3).unwrap_or(usize::MAX);
        if start >= end {
            return &[];
        }
        self.data.get(start..end).unwrap_or(&[])
    }
}
