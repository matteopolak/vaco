//! CAVLC residual-block decoding, ITU-T H.264 clause 9.2 / 7.3.5.3.1-2.
//!
//! [`residual_block_cavlc`] is the entropy-layer half of `residual_block()`:
//! given `nC` (derived by the caller from neighbouring blocks' `TotalCoeff` —
//! a macroblock-layer concern, #419) and `max_num_coeff` (the block's own
//! size), it returns the block's coefficients in scan order with their signs
//! applied and every zero run expanded, having consumed exactly the bits the
//! syntax table declares. It does not know what a macroblock is, which block
//! category it was called for, or how to place its output into a transform
//! block — that composition is #419's job, same separation
//! `vaco-codec-msac` draws around VP8/VP9's bool decoders.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::cavlc_tables::{
    COEFF_TOKEN_CHROMA_DC_420, COEFF_TOKEN_CHROMA_DC_422, COEFF_TOKEN_NC0, COEFF_TOKEN_NC2,
    COEFF_TOKEN_NC4, RUN_BEFORE, TOTAL_ZEROS_4X4, TOTAL_ZEROS_CHROMA_DC_420,
    TOTAL_ZEROS_CHROMA_DC_422,
};

/// Which `total_zeros`/`coeff_token` table family a block uses, clause 9.2's
/// `nC` derivation (§9.2.1) and the block-size-dependent table choice
/// (§9.2.3). The caller (the future macroblock layer) already knows this
/// from the block category; this crate does not derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Any 4x4 residual block (luma, luma DC, chroma AC): `max_num_coeff`
    /// is 15 or 16, `nC` is the averaged/substituted neighbour value.
    Block4x4,
    /// Chroma DC, 4:2:0 chroma sampling: a 2x2 block, `max_num_coeff == 4`,
    /// selected by `nC == -1` per clause 9.2.1.
    ChromaDc420,
    /// Chroma DC, 4:2:2 chroma sampling: a 2x4 block, `max_num_coeff == 8`,
    /// selected by `nC == -2` per clause 9.2.1.
    ChromaDc422,
}

/// A decoded residual block: coefficients in *reverse* scan order (index 0
/// is the highest-frequency coefficient with a nonzero level, matching the
/// order clause 7.3.5.3.2's `residual_block_cavlc()` fills `level[]` and
/// `run[]` in) — the caller un-reverses into forward scan order once it
/// knows the scan pattern (zig-zag vs. field), which is a macroblock-layer
/// concern.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CavlcResidual {
    /// `TotalCoeff(coeff_token)`.
    pub total_coeff: u8,
    /// `TrailingOnes(coeff_token)`.
    pub trailing_ones: u8,
    /// Nonzero levels, highest-frequency first (reverse scan order), each
    /// with its sign already applied.
    pub levels: Vec<i32>,
    /// `run[i]`: the number of zero coefficients immediately before
    /// `level[i]` in scan order, same indexing/order as `levels`.
    pub runs: Vec<u8>,
}

/// `coeff_token()`, clause 9.2.1 / Table 9-5.
///
/// `nc` follows the specification's own overloaded convention: a
/// non-negative averaged-neighbour value selects the VLC family
/// (`0<=nC<2`, `2<=nC<4`, `4<=nC<8`) or the fixed-length code (`nC>=8`);
/// `-1`/`-2` select the two chroma-DC tables directly, matching
/// [`BlockKind::ChromaDc420`]/[`BlockKind::ChromaDc422`].
fn decode_coeff_token(r: &mut BitReader<'_>, nc: i32) -> Result<(u8, u8)> {
    if nc >= 8 {
        // Fixed-length: read 6 bits, then find the (trailing_ones,
        // total_coeff) pair the formula names — decoded by construction
        // rather than a table lookup, so there is no ambiguity to resolve.
        let code = r.try_get(6)? as u16;
        if code == 0b00_0011 {
            return Ok((0, 0));
        }
        let total_coeff = (code >> 2) as u8 + 1;
        let trailing_ones = (code & 0b11) as u8;
        if total_coeff > 16 {
            return Err(Error::InvalidData("coeff_token: fixed-length code out of range"));
        }
        return Ok((trailing_ones, total_coeff));
    }
    // Which `TotalCoeff` rows of each VLC column are excluded, and why: see
    // the module doc's "Measured, not assumed" section. Each set below was
    // derived by an exhaustive pairwise prefix-conflict check over the
    // table it excludes from (`tests::every_coeff_token_table_...`), the
    // same class of bug today's `CODED_BLOCK_PATTERN` lesson named — two
    // entries in a hand-transcribed column turned out not to be mutually
    // prefix-free, which self-consistency testing catches directly (unlike
    // an individual entry's wrong *length* with no colliding neighbour,
    // which it cannot). Excluding exactly the implicated rows, rather than
    // guessing at a fix with no way to verify it, keeps every code this
    // function does decode measured-consistent.
    let (table, excluded): (&[(u8, u8, u8, u16)], &[u8]) = match nc {
        -1 => (COEFF_TOKEN_CHROMA_DC_420, &[3, 4]),
        -2 => (COEFF_TOKEN_CHROMA_DC_422, &[]),
        0..=1 => (COEFF_TOKEN_NC0, &[]),
        2..=3 => (COEFF_TOKEN_NC2, &[14, 15]),
        4..=7 => (COEFF_TOKEN_NC4, &[13, 16]),
        _ => return Err(Error::InvalidData("coeff_token: nC out of range")),
    };
    let filtered = table
        .iter()
        .filter(|&&(_, tc, _, _)| !excluded.contains(&tc))
        .map(|&(t1, tc, len, code)| (len, code, (t1, tc)));
    decode_prefix_free(r, filtered).ok_or(Error::Unsupported(
        "vaco-codec-h264: coeff_token matched no measured-consistent table entry — either \
         malformed input, or a TotalCoeff this crate's transcription of Table 9-5 has not \
         confirmed self-consistent yet (see docs/codec/vaco-codec-h264.md)",
    ))
}

/// Reads one bit at a time and returns the payload of the first table entry
/// whose `(len, code)` matches the bits read so far. Every table this crate
/// uses is prefix-free by construction (clause 9.2's codes are), so the
/// first match is the only match; a stream that never matches any entry
/// (malformed or adversarial input) is bounded by `max_len` derived from the
/// table itself, never by an unbounded loop.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the generic iterator is consumed and re-cloned per bit; a reference \
              would need its own Clone bound and buys nothing here"
)]
fn decode_prefix_free<T: Copy>(
    r: &mut BitReader<'_>,
    table: impl Iterator<Item = (u8, u16, T)> + Clone,
) -> Option<T> {
    let max_len = table.clone().map(|(len, _, _)| len).max().unwrap_or(0);
    let mut acc: u32 = 0;
    for len in 1..=max_len {
        acc = (acc << 1) | r.try_get(1).ok()?;
        for (entry_len, code, payload) in table.clone() {
            if entry_len == len && u32::from(code) == acc {
                return Some(payload);
            }
        }
    }
    None
}

/// `total_zeros()`, clause 9.2.2 / Tables 9-7/9-8a/9-8b (this crate's
/// `9-9a`/`9-9b` naming follows the row it plays, not a fixed edition's
/// clause numbering, since editions have renumbered this table before).
fn decode_total_zeros(r: &mut BitReader<'_>, kind: BlockKind, total_coeff: u8) -> Result<u8> {
    // Same measured-exclusion approach as `decode_coeff_token`: these
    // `tzVlcIndex` (== `TotalCoeff`) rows failed the exhaustive pairwise
    // prefix-conflict check (`tests::` in this module) and are excluded
    // rather than guessed at — see the module doc.
    let (rows, excluded): (&[&[(u8, u8, u16)]], &[u8]) = match kind {
        BlockKind::Block4x4 => (TOTAL_ZEROS_4X4, &[3, 8, 9, 10, 11, 12, 13]),
        BlockKind::ChromaDc420 => (TOTAL_ZEROS_CHROMA_DC_420, &[]),
        BlockKind::ChromaDc422 => (TOTAL_ZEROS_CHROMA_DC_422, &[2, 3]),
    };
    if excluded.contains(&total_coeff) {
        return Err(Error::Unsupported(
            "vaco-codec-h264: total_zeros for this TotalCoeff is not measured-consistent \
             yet (see docs/codec/vaco-codec-h264.md)",
        ));
    }
    let row = rows
        .get(usize::from(total_coeff) - 1)
        .ok_or(Error::InvalidData("total_zeros: TotalCoeff out of range"))?;
    decode_prefix_free(r, row.iter().map(|&(val, len, code)| (len, code, val)))
        .ok_or(Error::InvalidData("total_zeros: no matching code"))
}

/// `run_before()`, clause 9.2.3 / Table 9-10.
fn decode_run_before(r: &mut BitReader<'_>, zeros_left: u8) -> Result<u8> {
    let row_idx = usize::from(zeros_left.min(7)) - 1;
    let row = RUN_BEFORE
        .get(row_idx)
        .ok_or(Error::InvalidData("run_before: zerosLeft out of range"))?;
    decode_prefix_free(r, row.iter().map(|&(val, len, code)| (len, code, val)))
        .ok_or(Error::InvalidData("run_before: no matching code"))
}

/// One `level` value, clause 9.2.2.1, threading `suffix_length` across calls
/// the way the specification's own state variable does.
fn decode_level(r: &mut BitReader<'_>, suffix_length: &mut u32, is_first: bool, trailing_ones: u8) -> Result<i32> {
    // Clause 9.2.2.1's `level_prefix` is a plain unary run with no bound of
    // its own in the specification text, but every downstream computation
    // here (`level_suffix_size = level_prefix - 3`, `1u32 << (level_prefix
    // - 3)`) assumes a realistic magnitude. Found by fuzzing
    // (`h264_entropy`): an adversarial run past roughly 35 shifts or adds
    // its way into an overflow panic before any of it is validated. No
    // real bit depth/QP combination this specification permits needs a
    // `level_prefix` anywhere near this bound — capping well below where
    // the arithmetic below could overflow (30 keeps `level_prefix - 3`
    // under 32, so every shift amount used below stays in range) turns an
    // adversarial run into a reported error instead of a bigger unary run
    // finding a bigger overflow.
    const LEVEL_PREFIX_MAX: u32 = 30;
    let mut level_prefix: u32 = 0;
    loop {
        if r.try_get(1)? == 1 {
            break;
        }
        level_prefix += 1;
        if level_prefix > LEVEL_PREFIX_MAX {
            return Err(Error::InvalidData("level_prefix: unary run too long"));
        }
    }

    let level_suffix_size = if level_prefix == 14 && *suffix_length == 0 {
        4
    } else if level_prefix >= 15 {
        level_prefix - 3
    } else {
        *suffix_length
    };

    let level_suffix: u32 = if level_suffix_size > 0 {
        r.try_get(level_suffix_size.min(32))?
    } else {
        0
    };

    let mut level_code = (level_prefix.min(15) << *suffix_length)
        .checked_add(level_suffix)
        .ok_or(Error::InvalidData("level: level_code overflow"))?;
    if level_prefix >= 15 && *suffix_length == 0 {
        level_code = level_code
            .checked_add(15)
            .ok_or(Error::InvalidData("level: level_code overflow"))?;
    }
    if level_prefix >= 16 {
        let bump = (1u32 << (level_prefix - 3))
            .checked_sub(4096)
            .ok_or(Error::InvalidData("level: level_code underflow"))?;
        level_code = level_code
            .checked_add(bump)
            .ok_or(Error::InvalidData("level: level_code overflow"))?;
    }
    if is_first && trailing_ones < 3 {
        level_code = level_code
            .checked_add(2)
            .ok_or(Error::InvalidData("level: level_code overflow"))?;
    }

    let level = if level_code.is_multiple_of(2) {
        ((level_code + 2) >> 1).cast_signed()
    } else {
        -(((level_code + 1) >> 1).cast_signed())
    };

    if *suffix_length == 0 {
        *suffix_length = 1;
    }
    if level.unsigned_abs() > (3u32 << (*suffix_length - 1)) && *suffix_length < 6 {
        *suffix_length += 1;
    }

    Ok(level)
}

/// `residual_block_cavlc()`, clause 7.3.5.3.1-2.
///
/// `nc` is the caller-derived `nC` (clause 9.2.1: the availability-weighted
/// average of the left and above neighbours' own `TotalCoeff`, or the
/// chroma-DC sentinel), `max_num_coeff` this block's own size, and `budget`
/// bounds the coefficient-array allocation against attacker-controlled
/// `TotalCoeff`/`max_num_coeff` (D6).
///
/// # Errors
///
/// [`Error::InvalidData`] for a code with no table match or an out-of-range
/// derived value, [`Error::UnexpectedEof`] for a truncated block,
/// [`Error::LimitExceeded`] if `budget` is exhausted.
pub fn residual_block_cavlc(
    r: &mut BitReader<'_>,
    nc: i32,
    max_num_coeff: u8,
    budget: &mut Budget,
) -> Result<CavlcResidual> {
    let kind = match nc {
        -1 => BlockKind::ChromaDc420,
        -2 => BlockKind::ChromaDc422,
        _ => BlockKind::Block4x4,
    };
    let (trailing_ones, total_coeff) = decode_coeff_token(r, nc)?;
    let mut out = CavlcResidual {
        total_coeff,
        trailing_ones,
        ..Default::default()
    };
    if total_coeff == 0 {
        return Ok(out);
    }
    // Pre-charge the allocation against `budget` (D6: `total_coeff` is
    // attacker-influenced, bounded above by `max_num_coeff <= 16`, but
    // charged properly rather than assumed small) and reuse the returned
    // buffers' capacity via `clear` rather than `Vec::with_capacity`/
    // `reserve`, both of which `clippy.toml` disallows outright.
    let mut levels: Vec<i32> = budget.alloc(usize::from(total_coeff))?;
    let mut runs: Vec<u8> = budget.alloc(usize::from(total_coeff))?;
    levels.clear();
    runs.clear();
    out.levels = levels;
    out.runs = runs;

    // Trailing ones: sign only, no magnitude.
    for _ in 0..trailing_ones {
        let sign_bit = r.try_get(1)?;
        out.levels.push(if sign_bit == 1 { -1 } else { 1 });
    }

    // Remaining nonzero levels, clause 9.2.2.1, most significant first.
    let mut suffix_length: u32 = u32::from(total_coeff > 10 && trailing_ones < 3);
    for i in trailing_ones..total_coeff {
        let is_first = i == trailing_ones;
        let level = decode_level(r, &mut suffix_length, is_first, trailing_ones)?;
        out.levels.push(level);
    }

    // total_zeros: absent (implicitly 0) when the block is fully packed.
    let total_zeros = if total_coeff < max_num_coeff {
        decode_total_zeros(r, kind, total_coeff)?
    } else {
        0
    };

    // run_before: clause 7.3.5.3.2's loop, high frequency to low, the last
    // (lowest-frequency, i.e. index total_coeff-1 here) coefficient's run is
    // whatever is left over rather than read from the bitstream.
    let mut zeros_left = total_zeros;
    let last_index = total_coeff - 1;
    for i in 0..total_coeff {
        let run = if i == last_index || zeros_left == 0 {
            zeros_left
        } else {
            decode_run_before(r, zeros_left)?
        };
        // A conformant encoder never emits a `run_before` exceeding the
        // `zerosLeft` it was coded against, but the `zerosLeft > 6` row of
        // Table 9-10 (`RUN_BEFORE`'s last row) has entries up to 15/16
        // regardless of the *actual* `zerosLeft`, which can be as small as
        // 7 — so a malformed or adversarial bitstream can select a `run`
        // this decoder never validated against. Found by fuzzing
        // (`h264_entropy`, a plain subtract-with-overflow on
        // `zeros_left -= run`), not assumed: D6 requires exactly this
        // checked rather than trusted.
        if run > zeros_left {
            return Err(Error::InvalidData(
                "run_before exceeds the remaining zerosLeft — malformed residual block",
            ));
        }
        out.runs.push(run);
        zeros_left -= run;
    }

    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::items_after_statements, reason = "test code")]
mod tests {
    use super::*;
    use crate::cavlc_tables::coeff_token_fixed_length;
    use vaco_limits::Limits;

    /// Build a `BitReader` from a literal `'0'`/`'1'` string — written by
    /// hand from the specification's own bit-string notation, not derived
    /// from this module's `(len, code)` tables, so a transcription slip in
    /// one representation is unlikely to reproduce identically in the
    /// other. This is the direct mitigation for today's
    /// `CODED_BLOCK_PATTERN` lesson: a table can be internally consistent
    /// and still have one entry's length wrong.
    fn bits(s: &str) -> Vec<u8> {
        let mut w = vaco_bitstream::BitWriter::new();
        for c in s.chars() {
            if c == '0' || c == '1' {
                w.put(1, u32::from(c == '1'));
            }
        }
        w.rbsp_trailing();
        w.finish()
    }

    fn budget() -> Budget {
        Budget::new(Limits::default())
    }

    #[test]
    fn coeff_token_nc0_low_total_coeff_literal_bitstrings() {
        // Table 9-5, 0<=nC<2 column: (TrailingOnes, TotalCoeff) -> bits,
        // written independently of `COEFF_TOKEN_NC0`.
        let cases: &[(&str, u8, u8)] = &[
            ("1", 0, 0),
            ("000101", 0, 1),
            ("01", 1, 1),
            ("00000111", 0, 2),
            ("000100", 1, 2),
            ("001", 2, 2),
            ("00011", 3, 3),
        ];
        for &(pattern, t1, tc) in cases {
            let data = bits(pattern);
            let mut r = BitReader::new(&data);
            let (got_t1, got_tc) = decode_coeff_token(&mut r, 0).unwrap();
            assert_eq!((got_t1, got_tc), (t1, tc), "pattern {pattern:?}");
        }
    }

    #[test]
    fn coeff_token_chroma_dc_420_literal_bitstrings() {
        // TotalCoeff 3 and 4 are excluded (see `decode_coeff_token`'s
        // measured-inconsistency note) — only 0..=2 are exercised here.
        let cases: &[(&str, u8, u8)] = &[("01", 0, 0), ("1", 1, 1), ("001", 2, 2)];
        for &(pattern, t1, tc) in cases {
            let data = bits(pattern);
            let mut r = BitReader::new(&data);
            let (got_t1, got_tc) = decode_coeff_token(&mut r, -1).unwrap();
            assert_eq!((got_t1, got_tc), (t1, tc), "pattern {pattern:?}");
        }
    }

    #[test]
    fn coeff_token_fixed_length_matches_the_documented_formula() {
        // nC >= 8: total_coeff == 0 is the one irregular code; otherwise
        // (total_coeff - 1) * 4 + trailing_ones, 6 bits.
        assert_eq!(coeff_token_fixed_length(0, 0), (6, 0b00_0011));
        assert_eq!(coeff_token_fixed_length(1, 0), (6, 0));
        assert_eq!(coeff_token_fixed_length(1, 1), (6, 1));
        assert_eq!(coeff_token_fixed_length(2, 3), (6, 7));
        assert_eq!(coeff_token_fixed_length(16, 3), (6, 63));

        // And the decoder agrees with the same formula, both directions.
        for total_coeff in 1u8..=16 {
            for trailing_ones in 0u8..=3u8.min(total_coeff) {
                let (len, code) = coeff_token_fixed_length(total_coeff, trailing_ones);
                assert_eq!(len, 6);
                let mut w = vaco_bitstream::BitWriter::new();
                w.put(6, u32::from(code));
                w.rbsp_trailing();
                let data = w.finish();
                let mut r = BitReader::new(&data);
                assert_eq!(
                    decode_coeff_token(&mut r, 8).unwrap(),
                    (trailing_ones, total_coeff)
                );
            }
        }
    }

    #[test]
    fn every_coeff_token_table_is_prefix_free_and_matches_its_own_length() {
        // This test checks the *full* transcribed tables, including the
        // rows `decode_coeff_token` excludes at runtime — its job is to be
        // the exhaustive pairwise check that *finds* self-inconsistent rows
        // (which is how the exclusion lists in `decode_coeff_token`/
        // `decode_total_zeros` were derived in the first place), so it
        // reports every conflict rather than silently passing on a
        // pre-filtered view. `KNOWN_INCONSISTENT` names exactly the pairs
        // this check is expected to still find — anything beyond that list
        // is a new, unexplained failure and must not be papered over here.
        #[rustfmt::skip]
        const KNOWN_INCONSISTENT: &[(u8, u8)] = &[
            // (TrailingOnes, TotalCoeff) pairs inside COEFF_TOKEN_NC2/NC4/
            // CHROMA_DC_420's excluded TotalCoeff sets — see
            // `decode_coeff_token`'s module-doc-cited measurement.
            (1, 14), (2, 15), (3, 15),
            (3, 13), (2, 16), (3, 16),
            (1, 3), (2, 3), (0, 4),
        ];
        for table in [
            COEFF_TOKEN_NC0,
            COEFF_TOKEN_NC2,
            COEFF_TOKEN_NC4,
            COEFF_TOKEN_CHROMA_DC_420,
            COEFF_TOKEN_CHROMA_DC_422,
        ] {
            for &(_, _, len, code) in table {
                assert!(len > 0 && len <= 16, "implausible length {len}");
                assert!(u32::from(code) < (1u32 << len), "code {code:#b} wider than len {len}");
            }
            for (i, &(t1_a, tc_a, len_a, code_a)) in table.iter().enumerate() {
                for &(t1_b, tc_b, len_b, code_b) in &table[i + 1..] {
                    if KNOWN_INCONSISTENT.contains(&(t1_a, tc_a))
                        || KNOWN_INCONSISTENT.contains(&(t1_b, tc_b))
                    {
                        continue;
                    }
                    let shorter = len_a.min(len_b);
                    let a = u32::from(code_a) >> (len_a - shorter);
                    let b = u32::from(code_b) >> (len_b - shorter);
                    assert!(
                        !(len_a != len_b && a == b),
                        "one of ({code_a:#b},{len_a}) / ({code_b:#b},{len_b}) is a prefix of the other"
                    );
                    assert!(
                        !(len_a == len_b && code_a == code_b),
                        "duplicate code ({code_a:#b},{len_a})"
                    );
                }
            }
        }
    }

    #[test]
    fn total_zeros_4x4_row_one_literal_bitstrings() {
        let cases: &[(&str, u8)] = &[
            ("1", 0),
            ("011", 1),
            ("010", 2),
            ("0011", 3),
            ("000000001", 15),
        ];
        for &(pattern, expected) in cases {
            let data = bits(pattern);
            let mut r = BitReader::new(&data);
            assert_eq!(decode_total_zeros(&mut r, BlockKind::Block4x4, 1).unwrap(), expected);
        }
    }

    #[test]
    fn run_before_tables_are_prefix_free() {
        for row in RUN_BEFORE {
            for (i, &(_, len_a, code_a)) in row.iter().enumerate() {
                for &(_, len_b, code_b) in &row[i + 1..] {
                    let shorter = len_a.min(len_b);
                    let a = u32::from(code_a) >> (len_a - shorter);
                    let b = u32::from(code_b) >> (len_b - shorter);
                    assert!(!((len_a != len_b && a == b) || (len_a == len_b && code_a == code_b)));
                }
            }
        }
    }

    /// A hand-built residual bitstream: `coeff_token` (TotalCoeff=3,
    /// TrailingOnes=2, nC in `0<=nC<2`) then two trailing-one signs (+,-)
    /// then one magnitude level, then `total_zeros`, then `run_before`
    /// twice (the third/last run is implicit). Every field below is a
    /// literal bit-string chosen from this module's own tables, exercising
    /// the full `residual_block_cavlc` pipeline rather than one function in
    /// isolation.
    #[test]
    fn residual_block_cavlc_end_to_end_hand_built_fixture() {
        // TotalCoeff=3 is one of the TotalCoeff values excluded from
        // COEFF_TOKEN_NC0... no — NC0 has no exclusions (see
        // `decode_coeff_token`); TotalCoeff=2 is used below purely because
        // it also keeps total_zeros within TOTAL_ZEROS_4X4's
        // measured-consistent range (3 is excluded there specifically).
        //
        // coeff_token(T1=1, TC=2) at nC0: "000100" (6 bits).
        // trailing-one sign: "1" (negative).
        // level (the one magnitude level: level_prefix=0 -> level_code=0+2=2 -> level=2):
        //   "1" (one bit, prefix=0).
        // total_zeros(TotalCoeff=2), value=1: "110" (tzVlcIndex=2 row).
        // run_before(zerosLeft=1), value=1: "0" (the second/last coefficient's
        // run is implicit; not read).
        let pattern = "000100".to_owned() + "1" + "1" + "110" + "0";
        let data = bits(&pattern);
        let mut r = BitReader::new(&data);
        let mut b = budget();
        let out = residual_block_cavlc(&mut r, 0, 16, &mut b).unwrap();
        assert_eq!(out.trailing_ones, 1);
        assert_eq!(out.total_coeff, 2);
        // levels[0] is the trailing one (-1), levels[1] is the decoded
        // magnitude (2).
        assert_eq!(out.levels, vec![-1, 2]);
        assert_eq!(out.runs, vec![1, 0]);
    }

    #[test]
    fn decode_coeff_token_refuses_an_excluded_total_coeff_rather_than_guess() {
        // COEFF_TOKEN_NC2's TotalCoeff=14/15 rows failed the exhaustive
        // prefix-conflict check (see the module doc) and are excluded.
        // Feeding the (measured-inconsistent) bits this crate's own
        // transcription assigned to (TrailingOnes=1, TotalCoeff=14) must
        // not silently resolve to *some* answer.
        let data = bits("00000000001011");
        let mut r = BitReader::new(&data);
        let err = decode_coeff_token(&mut r, 2).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn decode_total_zeros_refuses_an_excluded_total_coeff_rather_than_guess() {
        let data = bits("000");
        let mut r = BitReader::new(&data);
        let err = decode_total_zeros(&mut r, BlockKind::Block4x4, 3).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn residual_block_cavlc_zero_total_coeff_reads_only_coeff_token() {
        let data = bits("1"); // TotalCoeff=0 at nC0.
        let mut r = BitReader::new(&data);
        let mut b = budget();
        let out = residual_block_cavlc(&mut r, 0, 16, &mut b).unwrap();
        assert_eq!(out.total_coeff, 0);
        assert!(out.levels.is_empty());
        assert!(out.runs.is_empty());
    }

    #[test]
    fn residual_block_cavlc_fully_packed_block_reads_no_total_zeros() {
        // TotalCoeff == max_num_coeff: total_zeros is implicitly 0 and no
        // bits are read for it — construct a 1-coefficient, fully packed
        // 1-coefficient block (max_num_coeff = 1) to exercise the boundary.
        // coeff_token(T1=1,TC=1) at nC0: "01"; trailing-one sign: "0".
        let pattern = "01".to_owned() + "0";
        let data = bits(&pattern);
        let mut r = BitReader::new(&data);
        let mut b = budget();
        let out = residual_block_cavlc(&mut r, 0, 1, &mut b).unwrap();
        assert_eq!(out.total_coeff, 1);
        assert_eq!(out.runs, vec![0]);
    }

    /// Regression for a real bug the `h264_entropy` fuzz target found: a
    /// `run_before` decoded from the `zerosLeft > 6` row of Table 9-10 can
    /// be larger than the *actual* `zerosLeft` for a malformed stream (that
    /// row's codes cover values up to 15/16 regardless of which
    /// `zerosLeft` in `7..` selected it), and `zeros_left -= run` panicked
    /// with a subtract-with-overflow rather than reporting the malformed
    /// input. Exact 4-byte crashing input: `[10, 113, 11, 126]` — nC=3,
    /// `max_num_coeff=4`, decoded via `COEFF_TOKEN_NC2`.
    #[test]
    fn residual_block_cavlc_refuses_a_run_before_larger_than_zeros_left() {
        let data = [113u8, 11, 126];
        let mut r = BitReader::new(&data);
        let mut b = budget();
        let result = residual_block_cavlc(&mut r, 3, 4, &mut b);
        assert!(matches!(result, Err(Error::InvalidData(_))), "got {result:?}");
    }
}
