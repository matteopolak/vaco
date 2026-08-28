//! The macroblock-type decode shape shared by every family here (D-22a): a
//! single VLC read per macroblock that yields a small, family-independent
//! set of flags governing what else the macroblock layer must read.
//!
//! H.261's `MTYPE`, H.263's Table 9, MPEG-1/2's Tables 6-17/6-18/6-19, and
//! MPEG-4 Part 2's Tables B-2/B-3 are all built around the same handful of
//! questions — is this macroblock intra-coded, does it carry a
//! `coded_block_pattern`, does it carry a quantiser-scale change, does it
//! predict forward and/or backward — even though the VLC tables themselves
//! (bit patterns, code lengths, which combinations exist at all) differ
//! completely per family and per picture type. This module is deliberately
//! *only* that shared shape: a flags struct plus a thin wrapper over
//! [`vaco_codec_vlc::VlcTable`] pairing each codeword with its own flags. No
//! table data lives here — supplying the table is exactly the part D-22
//! leaves to each family, per its own brief ("factor the shared skeleton so
//! each codec supplies its own tables and header parsing").
//!
//! A trait was deliberately *not* used here: a family's own decode loop
//! wants to match on which flags came back and branch accordingly (read a
//! quantiser-scale code if `quant`, read forward vectors if
//! `motion_forward`, ...), and a concrete struct is what makes that a plain
//! field read at the call site instead of a trait-object dispatch or a
//! generic type parameter threaded through the whole macroblock loop for no
//! benefit — the "lowest common denominator API" this crate's brief warns
//! against.

use vaco_bitstream::BitReader;
use vaco_codec_vlc::{VlcEntry, VlcTable};

/// The flags a macroblock-type codeword carries, common across the family.
/// A family whose own table needs an extra bit of information (MPEG-4 Part
/// 2's 4MV mode, say) carries it alongside this struct in its own row type
/// rather than this crate growing a field only one family ever sets — see
/// the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-meaningful syntax question a macroblock-type codeword answers (H.262 Table 6-17 and its H.263/MPEG-4 counterparts all expose exactly this vocabulary as separate bits), not a state machine in disguise"
)]
pub struct MbTypeFlags {
    /// The macroblock is intra-coded (no motion-compensated prediction).
    pub intra: bool,
    /// A `coded_block_pattern` (or equivalent) follows, selecting which
    /// blocks carry residual data.
    pub pattern: bool,
    /// A quantiser-scale (or equivalent) change follows.
    pub quant: bool,
    /// Forward motion vector(s) follow.
    pub motion_forward: bool,
    /// Backward motion vector(s) follow (B-pictures only, in every family
    /// that has them).
    pub motion_backward: bool,
}

/// One row of a family's own macroblock-type table: a VLC codeword paired
/// with the flags it decodes to.
#[derive(Debug, Clone, Copy)]
pub struct MbTypeEntry {
    /// The codeword itself, in [`vaco_codec_vlc::VlcEntry`]'s
    /// right-justified `(code, len)` form. `symbol` is unused (the flags
    /// are carried in `flags` instead) and should be `0`.
    pub entry: VlcEntry,
    /// What this codeword means.
    pub flags: MbTypeFlags,
}

impl MbTypeEntry {
    /// Build a row from a codeword's bits/length and its flags.
    #[must_use]
    pub const fn new(code: u32, len: u8, flags: MbTypeFlags) -> Self {
        Self {
            entry: VlcEntry::new(code, len, 0),
            flags,
        }
    }
}

/// Decode one macroblock-type codeword against a family-supplied table,
/// returning the flags it carries. `None` on a bitstream that matches no row
/// within the table's own longest codeword — a corrupt or adversarial
/// stream, never a conforming one, provided the table itself is prefix-free
/// (checked by `cargo xtask vlc-scan` and, per-entry, by
/// [`vaco_codec_vlc::is_prefix_free`]/[`vaco_codec_vlc::kraft_numerator`] in
/// the family's own tests).
///
/// Delegates the actual bit-matching to [`VlcTable::decode`] rather than
/// re-deriving a prefix-code scan here: each row's position in `rows`
/// becomes its `symbol` for exactly one call, so the shared engine does the
/// work and this function only translates the returned index back into the
/// caller's own [`MbTypeFlags`].
#[must_use]
pub fn decode_mb_type(r: &mut BitReader<'_>, rows: &[MbTypeEntry]) -> Option<MbTypeFlags> {
    // Bounded by the caller's own table size, which is a small, fixed,
    // compile-time-known constant in every real family table (at most a few
    // dozen rows) — never attacker-controlled, so this is not the "size an
    // allocation from an option" hazard `vaco_limits::Budget` exists to
    // guard against.
    let mut entries = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let index = u32::try_from(i).unwrap_or(u32::MAX);
        entries.push(VlcEntry::new(row.entry.code, row.entry.len, index));
    }
    let table = VlcTable::new(&entries);
    let symbol = table.decode(r)?;
    rows.get(usize::try_from(symbol).unwrap_or(usize::MAX))
        .map(|row| row.flags)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use vaco_bitstream::{BitReader, BitWriter};

    /// A tiny three-row table, prefix-free and complete: `1` -> intra, `01`
    /// -> forward-only, `00` -> forward+pattern.
    fn table() -> Vec<MbTypeEntry> {
        vec![
            MbTypeEntry::new(0b1, 1, MbTypeFlags { intra: true, ..MbTypeFlags::default() }),
            MbTypeEntry::new(
                0b01,
                2,
                MbTypeFlags { motion_forward: true, ..MbTypeFlags::default() },
            ),
            MbTypeEntry::new(
                0b00,
                2,
                MbTypeFlags { motion_forward: true, pattern: true, ..MbTypeFlags::default() },
            ),
        ]
    }

    #[test]
    fn decodes_every_row() {
        let rows = table();
        let mut w = BitWriter::new();
        w.put(1, 0b1);
        w.put(2, 0b01);
        w.put(2, 0b00);
        w.align_zero();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_mb_type(&mut r, &rows), Some(rows[0].flags));
        assert_eq!(decode_mb_type(&mut r, &rows), Some(rows[1].flags));
        assert_eq!(decode_mb_type(&mut r, &rows), Some(rows[2].flags));
    }

    #[test]
    fn unmatched_prefix_returns_none_and_consumes_nothing() {
        let rows = vec![MbTypeEntry::new(0b111, 3, MbTypeFlags::default())];
        let bytes = [0u8; 2];
        let mut r = BitReader::new(&bytes);
        let before = r.bit_pos();
        assert_eq!(decode_mb_type(&mut r, &rows), None);
        assert_eq!(r.bit_pos(), before);
    }

    #[test]
    fn empty_table_never_matches() {
        let rows: Vec<MbTypeEntry> = Vec::new();
        let bytes = [0xffu8; 2];
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_mb_type(&mut r, &rows), None);
    }

    proptest::proptest! {
        /// Arbitrary bytes against an arbitrary (not necessarily
        /// prefix-free) table must never panic — a family's own table is
        /// exactly the kind of thing this function cannot trust to be
        /// well-formed if a bug ever produces one that is not.
        #[test]
        fn never_panics_on_an_arbitrary_table_and_input(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..32),
            codes in proptest::collection::vec((proptest::prelude::any::<u32>(), 0u8..=20), 0..8),
        ) {
            let rows: Vec<MbTypeEntry> = codes
                .into_iter()
                .map(|(code, len)| MbTypeEntry::new(code, len, MbTypeFlags::default()))
                .collect();
            let mut r = BitReader::new(&data);
            let _ = decode_mb_type(&mut r, &rows);
        }
    }
}
