//! Variable-length ("Huffman"/VLC) code tables and readers (D-01).
//!
//! # Why this crate exists
//!
//! `vaco-codec-mpegaudio` and `vaco-codec-ac3` each grew their own linear-scan
//! prefix-code reader for a table transcribed straight out of a specification
//! (MP3's 32 "big values" tables plus two `count1` tables in
//! `huffman.rs`/`huffman_data.rs`). That is the same small mechanism written
//! twice, and AAC's spectral data needs a third copy — 12 real Huffman
//! codebooks (ISO/IEC 14496-3 subpart 4 Table 4.69/4.70) — plus a fourth for
//! its scalefactors. This crate is that mechanism, written once.
//!
//! (One claim in the brief that dispatched this crate did not survive contact
//! with the code: AC-3's mantissa decoding, in
//! `vaco-codec-ac3/src/mantissa.rs`, is fixed-width grouped-radix
//! decomposition — `group_code = digit[0]*levels^(count-1) + ...` — not a
//! variable-length code at all. There is no second Huffman table to point to
//! there. MP3's is real, and AAC's are real, and that is reason enough for
//! this crate.)
//!
//! # What a [`VlcTable`] is
//!
//! A flat list of [`VlcEntry`] — `(code, len, symbol)` triples, transcribed
//! directly from a specification's own codeword/length columns — searched by
//! **linear scan**: peek this table's longest codeword's worth of bits,
//! right-shift each candidate entry's code to the peeked width, and compare.
//! Correctness-first, exactly as MP3's own tables are today (`huffman.rs`'s
//! own words: "a real decode tree is future work"). A future faster
//! implementation can replace the scan inside [`VlcTable::decode`] without
//! moving a single table, because the tables are just data.
//!
//! # The two checks every transcribed table should pass
//!
//! Transcription errors are exactly the failure mode this crate defends
//! against — a spec's own printed table, retyped by hand or extracted from a
//! scanned PDF, is where a single flipped bit hides. [`is_prefix_free`] and
//! [`kraft_numerator`] are the same two-part check MP3's own huffman tables
//! were held to before this crate existed (see that crate's
//! `every_table_is_a_complete_prefix_free_code`): no codeword may be a prefix
//! of another (prefix-free, checked structurally, without needing to know any
//! codeword in advance), and — for codes the spec states are *complete* —
//! `Σ 2^-len == 1`.
//!
//! # Safety on untrusted input
//!
//! [`VlcTable::decode`] never loops: it does one bounded `peek`, one linear
//! scan over a fixed, compile-time-sized table, and one `skip`. A bitstream
//! whose next codeword matches no entry returns `None` rather than consuming
//! anything or panicking — the caller decides whether that is a corrupt
//! stream or (for an incomplete code) a reserved value.

use vaco_bitstream::BitReader;

/// One entry of a VLC table: a codeword, its bit length, and the symbol it
/// decodes to.
///
/// `code` is right-justified: for a 5-bit codeword `01101`, `code == 0b01101`
/// and `len == 5`, exactly as it reads off the specification's own table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VlcEntry {
    /// The codeword, right-justified in the low `len` bits.
    pub code: u32,
    /// The codeword's length in bits. `1..=32`; a `len` of 0 is never
    /// produced by [`VlcTable::new`]'s `max_len` computation and is simply
    /// skipped by [`VlcTable::decode`], since a zero-length "codeword" cannot
    /// be distinguished from any other by peeking.
    pub len: u8,
    /// The symbol this codeword decodes to. Callers map this back to
    /// whatever domain-specific meaning the table gives it (a `run/level`
    /// pair, a scalefactor delta, ...); this crate does not interpret it.
    pub symbol: u32,
}

impl VlcEntry {
    /// Build an entry. `code` is right-justified, as described on the
    /// struct's own docs.
    #[must_use]
    pub const fn new(code: u32, len: u8, symbol: u32) -> Self {
        Self { code, len, symbol }
    }
}

/// A variable-length code table, ready to decode against a [`BitReader`].
///
/// Borrows its entries rather than owning them, so a table transcribed as a
/// `static` (as every table in this workspace's other codec crates already
/// is, per D6/D7's "no runtime table generation from a shipped constant"
/// convention) costs nothing to wrap: `VlcTable::new(&STATIC_ARRAY)`.
#[derive(Debug, Clone, Copy)]
pub struct VlcTable<'a> {
    entries: &'a [VlcEntry],
    max_len: u8,
}

impl<'a> VlcTable<'a> {
    /// Build a table from its entries, computing the longest codeword's
    /// length once so [`decode`](Self::decode) knows how much to peek.
    ///
    /// The underlying table (a `static`/`const` array, per this workspace's
    /// usual convention for spec-transcribed data) costs nothing to borrow;
    /// this constructor itself is cheap — one pass over the entries — but is
    /// not `const` (a const-evaluable max-of-a-slice needs indexing, which
    /// `clippy::indexing_slicing` denies workspace-wide even inside a `const
    /// fn`). Call it once per decode call site, or hold the result in a local
    /// if a single symbol read calls [`decode`](Self::decode) more than once.
    #[must_use]
    pub fn new(entries: &'a [VlcEntry]) -> Self {
        let max_len = entries.iter().map(|e| e.len).max().unwrap_or(0);
        Self { entries, max_len }
    }

    /// The longest codeword in this table, in bits. `0` for an empty table.
    #[must_use]
    pub const fn max_len(&self) -> u8 {
        self.max_len
    }

    /// The number of entries in this table.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this table has no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decode one symbol, consuming exactly as many bits as its codeword's
    /// length on a match.
    ///
    /// Peeks `max_len` bits (without consuming them), right-shifts each
    /// candidate entry's `code` up to that width for comparison, and on the
    /// first match, consumes that entry's own `len` bits — not `max_len`.
    /// Entries are expected to be prefix-free (see [`is_prefix_free`]), so at
    /// most one can match a given `max_len`-bit prefix; if more than one
    /// would, the first in table order wins, which is a caller bug this
    /// crate cannot detect from inside `decode` alone. A zero-length entry is
    /// never a candidate (its `shift` would equal `max_len`, indistinguishable
    /// from every other entry's high bits) and is silently skipped.
    ///
    /// Returns `None` — consuming nothing — when no entry matches: either the
    /// stream is corrupt, or (for a table the specification defines as
    /// incomplete) this is a codeword the encoder never emits.
    pub fn decode(&self, r: &mut BitReader<'_>) -> Option<u32> {
        if self.max_len == 0 {
            return None;
        }
        let peeked = r.peek(u32::from(self.max_len));
        for entry in self.entries {
            if entry.len == 0 || entry.len > self.max_len {
                continue;
            }
            let shift = self.max_len - entry.len;
            if (peeked >> shift) == entry.code {
                r.skip(u32::from(entry.len));
                return Some(entry.symbol);
            }
        }
        None
    }
}

/// Whether a set of entries forms a prefix-free code: no codeword is a prefix
/// of another.
///
/// This is a **structural** check — it never needs to know what any codeword
/// "should" be, which is exactly why it catches transcription errors instead
/// of merely trusting them. Does not require the code to be *complete*; many
/// real codebooks (an escape code carved out, a reserved value) are
/// deliberately incomplete. Use [`kraft_numerator`] alongside this to also
/// check completeness where the specification states the code is complete.
#[must_use]
pub fn is_prefix_free(entries: &[VlcEntry]) -> bool {
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let (Some(a), Some(b)) = (entries.get(i), entries.get(j)) else {
                continue;
            };
            let (short, long) = if a.len <= b.len { (*a, *b) } else { (*b, *a) };
            if short.len == 0 || long.len == 0 || short.len == long.len {
                if short.len == long.len && short.code == long.code {
                    return false;
                }
                continue;
            }
            let shift = long.len - short.len;
            if (long.code >> shift) == short.code {
                return false;
            }
        }
    }
    true
}

/// The Kraft sum `Σ 2^(scale_len - len)`, i.e. `2^scale_len` times
/// `Σ 2^-len`.
///
/// A code with the maximum codeword length `scale_len` is **complete** — every
/// leaf of its binary tree is used — exactly when this returns `1 << scale_len`.
/// Returning it as an exact integer (rather than a float sum that could mask a
/// one-ULP transcription slip) is the point: compare it to `1u64 << scale_len`
/// with `==`, not an epsilon.
///
/// # Panics
///
/// Never: an entry whose `len` exceeds `scale_len` contributes `0` rather than
/// underflowing, since such an entry cannot belong to a code scaled to
/// `scale_len` in the first place (a caller comparing the result to
/// `1 << scale_len` will see the mismatch either way).
#[must_use]
pub fn kraft_numerator(entries: &[VlcEntry], scale_len: u8) -> u64 {
    let mut total: u64 = 0;
    for e in entries {
        if e.len == 0 || e.len > scale_len {
            continue;
        }
        total = total.saturating_add(1u64 << (scale_len - e.len));
    }
    total
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::{VlcEntry, VlcTable, is_prefix_free, kraft_numerator};
    use vaco_bitstream::{BitReader, BitWriter};

    /// A tiny complete code: `0`, `10`, `11` for symbols `A`, `B`, `C`.
    /// `Σ 2^-len == 1/2 + 1/4 + 1/4 == 1` — complete and prefix-free by
    /// inspection, so it doubles as a check that the two verifiers agree with
    /// a code a human can eyeball.
    const TOY: [VlcEntry; 3] = [
        VlcEntry::new(0b0, 1, 0),
        VlcEntry::new(0b10, 2, 1),
        VlcEntry::new(0b11, 2, 2),
    ];

    #[test]
    fn toy_code_is_prefix_free_and_complete() {
        assert!(is_prefix_free(&TOY));
        assert_eq!(kraft_numerator(&TOY, 2), 1 << 2);
    }

    #[test]
    fn a_prefix_collision_is_detected() {
        // "0" and "01" collide: "0" is a prefix of "01".
        let bad = [VlcEntry::new(0b0, 1, 0), VlcEntry::new(0b01, 2, 1)];
        assert!(!is_prefix_free(&bad));
    }

    #[test]
    fn an_incomplete_code_has_a_smaller_kraft_sum() {
        // Only "0" and "10" — "11" is unused. Prefix-free, but not complete.
        let incomplete = [VlcEntry::new(0b0, 1, 0), VlcEntry::new(0b10, 2, 1)];
        assert!(is_prefix_free(&incomplete));
        assert_eq!(kraft_numerator(&incomplete, 2), 3); // 2 + 1, not 4
    }

    #[test]
    fn decode_reads_every_symbol_of_the_toy_code() {
        let table = VlcTable::new(&TOY);
        assert_eq!(table.max_len(), 2);
        // Bitstream: "0" "10" "11" back to back == 0b01011, padded to a byte.
        let bytes = [0b0101_1000u8];
        let mut r = BitReader::new(&bytes);
        assert_eq!(table.decode(&mut r), Some(0));
        assert_eq!(table.decode(&mut r), Some(1));
        assert_eq!(table.decode(&mut r), Some(2));
    }

    #[test]
    fn decode_of_an_unmatched_prefix_consumes_nothing_and_returns_none() {
        // Table without a "1"-prefixed entry at all.
        let sparse = [VlcEntry::new(0b0, 1, 42)];
        let table = VlcTable::new(&sparse);
        let bytes = [0b1111_1111u8];
        let mut r = BitReader::new(&bytes);
        let before = r.bit_pos();
        assert_eq!(table.decode(&mut r), None);
        assert_eq!(r.bit_pos(), before, "a failed decode must not consume bits");
    }

    #[test]
    fn empty_table_never_matches_and_never_panics() {
        let empty: [VlcEntry; 0] = [];
        let table = VlcTable::new(&empty);
        assert_eq!(table.max_len(), 0);
        let bytes = [0xffu8; 4];
        let mut r = BitReader::new(&bytes);
        assert_eq!(table.decode(&mut r), None);
    }

    #[test]
    fn round_trip_through_a_real_bit_writer() {
        let table = VlcTable::new(&TOY);
        let mut w = BitWriter::new();
        for entry in &TOY {
            w.put(u32::from(entry.len), entry.code);
        }
        w.align_zero();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for entry in &TOY {
            assert_eq!(table.decode(&mut r), Some(entry.symbol));
        }
    }
}
