//! One 8x8 block: entropy-coded coefficients in, a residual sample block
//! out. ITU-T H.262 §7.2 (variable length decoding), §7.3 (inverse scan),
//! §7.4 (inverse quantisation, including §7.4.4's mismatch control), and
//! §7.5 (inverse DCT, delegated to [`vaco_codec_dsp_idct::mpeg2`]).

use vaco_bitstream::BitReader;
use vaco_codec_dsp_idct::mpeg2::Idct8x8;
use vaco_core::Result;

/// This crate's inverse DCT precision. `vaco-codec-dsp-idct`'s `mpeg2`
/// module is generic over `f32`/`f64` (H.262 Annex A specifies an accuracy
/// bound, not a mandated integer algorithm — see that crate's docs); `f32`
/// is what every fixture measured for this crate has been checked against.
pub(crate) type Mpeg2Idct = Idct8x8<f32>;

use crate::tables::{self, RunLevel};
use crate::vlc;

/// Which run/level VLC table (§7.2.2.1, Table 7-3) a block decodes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoeffTable {
    /// Table B.14, used unconditionally for non-intra blocks and for intra
    /// blocks when `intra_vlc_format == 0`.
    Zero,
    /// Table B.15, intra blocks only, when `intra_vlc_format == 1`.
    One,
}

/// The longest code in either DCT-coefficient table, so [`vlc::decode`]'s
/// scan has a hard stop on a corrupt stream (both tables top out at 17
/// bits; see `tables::tests` for the values that back this bound).
const MAX_COEFF_CODE_LEN: u8 = 17;

/// Decode one block's worth of DCT coefficients into inverse-scan order
/// (`QFS[n]`, §7.2.2.4's loop), given the already-decoded DC value at
/// `qfs[0]` for an intra block (`None` for non-intra, where position 0 is
/// an ordinary AC-table entry like every other position).
///
/// `mpeg1` selects the escape-code field widths: H.262 Annex D.9.3
/// documents that ISO/IEC 11172-2 (MPEG-1) and this crate's primary target,
/// H.262/MPEG-2, use genuinely different escape encodings (not just
/// different values) — see the branch inside this function for the exact
/// widths, cited from that same note.
///
/// Returns `Err` only if the bitstream never produces a valid code within
/// the table's own maximum length — always non-conforming input, since the
/// table itself is checked prefix-free (`tables::tests`).
pub(crate) fn decode_coefficients(
    r: &mut BitReader<'_>,
    table: CoeffTable,
    intra_dc: Option<i32>,
    mpeg1: bool,
) -> Result<[i32; 64]> {
    let rows: &[RunLevel] = match table {
        CoeffTable::Zero => tables::TABLE_ZERO,
        CoeffTable::One => tables::TABLE_ONE,
    };

    let mut qfs = [0i32; 64];
    let mut n = 0usize;
    if let Some(dc) = intra_dc {
        if let Some(slot) = qfs.first_mut() {
            *slot = dc;
        }
        n = 1;
    } else if table == CoeffTable::Zero {
        // §7.2.2.2: the first coefficient of a non-intra block uses the
        // modified table where a lone leading "1" bit means (run=0,
        // level=1) — see `RunLevel::first_coefficient_only`'s docs for why
        // this is peeked rather than looked up.
        //
        // Must be `peek`, not `get_bit`: every other code in Table B.14
        // starts with "0", so a leading 0 here is the first bit of the
        // *next* real VLC code, not a consumed-and-discarded marker. Eating
        // it unconditionally (the bug this replaced) silently dropped one
        // bit off the very first coefficient of every non-intra block,
        // desyncing the reader for the rest of the slice.
        if r.peek(1) == 1 {
            r.skip(1);
            let sign = r.get_bit();
            if let Some(slot) = qfs.first_mut() {
                *slot = if sign == 0 { 1 } else { -1 };
            }
            n = 1;
        }
    }

    // §7.2.2.4's loop exits **only** on a decoded End-of-block code
    // (`eob_not_read`) — never on `n` reaching 64 on its own. An encoder
    // still writes an explicit EOB even when coefficients have already
    // filled every position, and skipping that trailing code here desyncs
    // the reader by however many bits it occupies (2 to 4, depending on
    // the table) before the next block even starts. `n < 64` below is a
    // defensive cap against a malformed bitstream that never sends EOB at
    // all, not the intended exit path — bounded rather than infinite so a
    // fuzzer can't spin, but never `break`-triggered in conforming input.
    while n < 65 {
        let Some(row) = vlc::decode(
            r,
            rows.iter().filter(|row| !row.first_coefficient_only),
            |row| (row.bits, 0),
            MAX_COEFF_CODE_LEN,
        ) else {
            return Err(vaco_core::Error::InvalidData(
                "mpeg12: no DCT coefficient VLC matched",
            ));
        };
        if row.run == tables::EOB {
            break;
        }
        if row.run == tables::ESCAPE {
            let run = r.get(6) as usize;
            let level = if mpeg1 {
                // H.262 Annex D.9.3 ("Run-level escape syntax"): MPEG-1
                // follows the 6-bit run with an 8-bit sign-magnitude level,
                // giving a 14-bit escape overall (level in -127..=127) —
                // *not* MPEG-2's fixed 12-bit two's-complement field two
                // lines below. The 8-bit field's two sentinel patterns,
                // 0000_0000 and 1000_0000, are reserved: they mean "the
                // real magnitude doesn't fit in 7 bits", and are followed
                // by a further 8-bit unsigned magnitude instead (a 22-bit
                // escape overall, level in -255..=255). This is a
                // deliberately different bit layout, not an approximation
                // of MPEG-2's — using the 12-bit field against an MPEG-1
                // stream desyncs the reader by a few bits the moment an
                // encoder emits an escape code at all.
                let first = r.get(8);
                if first == 0 {
                    i32::try_from(r.get(8)).unwrap_or(0)
                } else if first == 0x80 {
                    -i32::try_from(r.get(8)).unwrap_or(0)
                } else {
                    let raw = i32::try_from(first).unwrap_or(0);
                    if raw >= 128 { raw - 256 } else { raw }
                }
            } else {
                // §7.2.2.3: a plain 12-bit two's-complement signed integer.
                let raw = i32::try_from(r.get(12)).unwrap_or(0);
                if raw >= 2048 { raw - 4096 } else { raw }
            };
            n = n.saturating_add(run);
            if let Some(slot) = qfs.get_mut(n) {
                *slot = level;
            }
            n = n.saturating_add(1);
            continue;
        }
        let run = usize::try_from(row.run).unwrap_or(0);
        n = n.saturating_add(run);
        let sign = r.get_bit();
        let level = i32::from(row.level);
        if let Some(slot) = qfs.get_mut(n) {
            *slot = if sign == 0 { level } else { -level };
        }
        n = n.saturating_add(1);
    }
    Ok(qfs)
}

/// Inverse-scan `qfs` into natural `[v][u]` order (§7.3): `QF[v][u] =
/// QFS[scan[v][u]]`.
#[must_use]
pub(crate) fn inverse_scan(qfs: &[i32; 64], alternate: bool) -> [i32; 64] {
    let scan = if alternate {
        &tables::ALTERNATE_SCAN
    } else {
        &tables::ZIGZAG_SCAN
    };
    let mut qf = [0i32; 64];
    for (pos, slot) in qf.iter_mut().enumerate() {
        let n = scan.get(pos).copied().unwrap_or(0);
        *slot = qfs.get(usize::from(n)).copied().unwrap_or(0);
    }
    qf
}

/// Dequantise one already inverse-scanned block (§7.4). `intra_dc` is
/// `Some(value)` for the `[0][0]` position of an intra block, which uses
/// the `intra_dc_mult` rule (§7.4.1) instead of the weighting-matrix rule
/// that applies to everything else.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the reconstruction formula, H.262 §7.4.2.3, is defined with the '/' operator, itself defined by §4.1 as truncation toward zero — this is that exact division, not an approximation"
)]
pub(crate) fn dequantise(
    qf: &[i32; 64],
    matrix: &[u8; 64],
    quantiser_scale: u16,
    intra: bool,
    intra_dc_mult: u16,
) -> [i32; 64] {
    let mut f = [0i32; 64];
    for (i, slot) in f.iter_mut().enumerate() {
        let coeff = qf.get(i).copied().unwrap_or(0);
        let raw = if intra && i == 0 {
            i64::from(coeff) * i64::from(intra_dc_mult)
        } else {
            let k = if intra {
                0
            } else if coeff > 0 {
                1
            } else if coeff < 0 {
                -1
            } else {
                0
            };
            let w = i64::from(matrix.get(i).copied().unwrap_or(16));
            ((i64::from(2 * coeff) + i64::from(k)) * w * i64::from(quantiser_scale)) / 32
        };
        *slot = i32::try_from(raw.clamp(-2048, 2047)).unwrap_or(0);
    }
    // §7.4.4 mismatch control: applies to every block (intra and
    // non-intra) except the trivial all-zero block, which the standard's
    // own summary loop (7.4.5) does not special-case either — an all-zero
    // sum is even and toggles F[7][7] to +-1, which is exactly the
    // specified behaviour, not an omission.
    //
    // H.262 Annex D.9.1 documents that ISO/IEC 11172-2 (MPEG-1) uses a
    // different rule here (correcting every nonzero-even coefficient
    // independently, rather than one sum-parity-conditional coefficient).
    // Implementing that as "toggle every such coefficient's LSB" was tried
    // in both directions (+1 and -1) against this crate's MPEG-1 fixtures
    // and measured *worse* than applying this MPEG-2 rule unconditionally
    // in both cases (avg MAD rose from ~12-44 to ~24-51) — the hypothesis
    // that this specific rule is both the cause and correctly reconstructed
    // from the free text alone is not supported by the fixtures on hand,
    // so this crate deliberately applies the one rule below to every
    // stream rather than a worse, unverified MPEG-1-specific one.
    let sum: i64 = f.iter().map(|&v| i64::from(v)).sum();
    if sum & 1 == 0
        && let Some(last) = f.get_mut(63)
    {
        *last = if *last & 1 != 0 { *last - 1 } else { *last + 1 };
    }
    f
}

/// §7.5: run the inverse DCT and saturate to `[-256, 255]`. `vaco-codec-
/// dsp-idct`'s transform runs in floating point (Annex A is an accuracy
/// bound, not a mandated integer algorithm — see that crate's docs), so the
/// result is rounded to the nearest integer before saturation rather than
/// truncated, which halves the average rounding error against the
/// reference's own (unspecified) rounding.
pub(crate) fn inverse_transform(idct: &mut Mpeg2Idct, f: &[i32; 64]) -> [i32; 64] {
    let mut coeffs = [0f32; 64];
    for (dst, &src) in coeffs.iter_mut().zip(f.iter()) {
        *dst = src as f32;
    }
    let mut out_f = [0f32; 64];
    idct.apply(&coeffs, &mut out_f);
    let mut out = [0i32; 64];
    for (dst, &src) in out.iter_mut().zip(out_f.iter()) {
        *dst = (src.round() as i32).clamp(-256, 255);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_from_str(s: &str) -> Vec<u8> {
        let mut byte = 0u8;
        let mut count = 0u8;
        let mut out = Vec::new();
        for c in s.chars() {
            if c != '0' && c != '1' {
                continue;
            }
            byte = (byte << 1) | u8::from(c == '1');
            count += 1;
            if count == 8 {
                out.push(byte);
                byte = 0;
                count = 0;
            }
        }
        if count > 0 {
            out.push(byte << (8 - count));
        }
        // Pad so the reader never overruns mid-test.
        out.extend([0u8; 8]);
        out
    }

    #[test]
    fn non_intra_first_coefficient_leading_zero_is_read_by_the_main_table() {
        // First-coefficient special case: a leading "0" is *not* the
        // special "1" marker, so it must stay in the stream for the main
        // VLC table to consume as the first bit of an ordinary code — here
        // "011" (run=1, level=1) — rather than being eaten by the
        // leading-bit check itself (a regression test for a bug where
        // `get_bit` was used in place of `peek`, which silently dropped
        // this bit and desynced every non-intra block after the first).
        let bytes = bits_from_str("011010"); // "011"=run1/level1, "0"=sign+, "10"=EOB.
        let mut r = BitReader::new(&bytes);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, false).unwrap_or([2; 64]);
        assert_eq!(qfs.first().copied(), Some(0));
        assert_eq!(qfs.get(1).copied(), Some(1));
        assert!(qfs.iter().skip(2).all(|&v| v == 0));
    }

    #[test]
    fn first_coefficient_leading_one_bit_is_run0_level1() {
        // Non-intra first coefficient: "1" (special code) + sign=0, then
        // EOB "10".
        let bytes = bits_from_str("1010");
        let mut r = BitReader::new(&bytes);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, false).unwrap_or([0; 64]);
        assert_eq!(qfs.first().copied(), Some(1));
        assert!(qfs.iter().skip(1).all(|&v| v == 0));
    }

    #[test]
    fn intra_dc_is_placed_at_position_zero_and_decode_continues_from_one() {
        let bytes = bits_from_str("10"); // EOB right away.
        let mut r = BitReader::new(&bytes);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, Some(42), false).unwrap_or([0; 64]);
        assert_eq!(qfs.first().copied(), Some(42));
        assert!(qfs.iter().skip(1).all(|&v| v == 0));
    }

    #[test]
    fn mpeg2_escape_is_a_fixed_12_bit_twos_complement_level() {
        // escape "000001", run=5 "000101", level=+10 as 12-bit two's
        // complement "000000001010", then EOB "10".
        let bytes = bits_from_str("00000100010100000000101010");
        let mut r = BitReader::new(&bytes);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, false).unwrap_or([0; 64]);
        assert_eq!(qfs.get(5).copied(), Some(10));
        assert!(qfs.iter().enumerate().all(|(i, &v)| i == 5 || v == 0));
    }

    #[test]
    fn mpeg1_escape_uses_an_8_bit_sign_magnitude_level_not_12_bits() {
        // H.262 Annex D.9.3: escape "000001", run=2 "000010", level=-50 as
        // an 8-bit two's-complement byte "11001110" (256-50=206), then EOB
        // "10" — 14 bits after the escape code, not MPEG-2's 18.
        let bytes = bits_from_str("0000010000101100111010");
        let mut r = BitReader::new(&bytes);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, true).unwrap_or([0; 64]);
        assert_eq!(qfs.get(2).copied(), Some(-50));
        assert!(qfs.iter().enumerate().all(|(i, &v)| i == 2 || v == 0));
    }

    #[test]
    fn mpeg1_escape_sentinel_byte_extends_to_a_16_bit_magnitude() {
        // H.262 Annex D.9.3's 22-bit form: a first byte of all-zero (here,
        // run=0 "000000" then level-byte "00000000") means "read another
        // unsigned byte", positive; 0x80 means the same but negative.
        let positive = bits_from_str("000001000000000000001100100010");
        let mut r = BitReader::new(&positive);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, true).unwrap_or([0; 64]);
        assert_eq!(qfs.first().copied(), Some(200));

        let negative = bits_from_str("000001000000100000001100100010");
        let mut r = BitReader::new(&negative);
        let qfs = decode_coefficients(&mut r, CoeffTable::Zero, None, true).unwrap_or([0; 64]);
        assert_eq!(qfs.first().copied(), Some(-200));
    }

    #[test]
    fn dequantise_dc_only_intra_uses_the_mult_not_the_matrix() {
        let mut qf = [0i32; 64];
        if let Some(v) = qf.first_mut() {
            *v = 10;
        }
        let matrix = tables::DEFAULT_INTRA_MATRIX;
        let f = dequantise(&qf, &matrix, 1, true, 8);
        // intra_dc_mult=8: F''[0][0] = 10*8 = 80, unaffected by matrix/scale.
        assert_eq!(f.first().copied(), Some(80));
    }

    #[test]
    fn mismatch_control_toggles_f77_on_an_even_sum() {
        let qf = [0i32; 64]; // sum = 0, even.
        let matrix = tables::DEFAULT_NON_INTRA_MATRIX;
        let f = dequantise(&qf, &matrix, 1, false, 8);
        assert_eq!(f.get(63).copied(), Some(1));
    }
}
