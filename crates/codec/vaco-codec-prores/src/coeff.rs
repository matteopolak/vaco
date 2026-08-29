//! Entropy decode of scanned DCT coefficients and alpha values — RDD 36
//! SS7.1.1 (`scanned_coefficients()`) and SS7.1.2 (`scanned_alpha()`).

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::golomb::{combo, exp_golomb, symbol_to_signed};

/// A Golomb-Rice/exponential-Golomb combination codebook, `(lastRiceQ,
/// kRice, kExp)`.
#[derive(Debug, Clone, Copy)]
struct Codebook(u32, u32, u32);

impl Codebook {
    fn decode(self, r: &mut BitReader<'_>) -> Result<u64> {
        combo(r, self.0, self.1, self.2)
    }
}

/// `EXP_GOLOMB_CODE(k)` written as the `Codebook` it is per SS7.1.1.1's own
/// closing note: `combo(0, k, k+1)`.
const fn exp(k: u32) -> Codebook {
    Codebook(0, k, k + 1)
}

/// Table 9: codebook adaptation for `dc_coeff_difference`, keyed by
/// `|previousDCDiff|`.
fn dc_diff_codebook(abs_previous: u64) -> Codebook {
    match abs_previous {
        0 => exp(0),
        1 => exp(1),
        2 => Codebook(1, 2, 3),
        _ => exp(3),
    }
}

/// Table 10: codebook adaptation for `run`, keyed by `previousRun`.
fn run_codebook(previous_run: u64) -> Codebook {
    match previous_run {
        0 | 1 => Codebook(2, 0, 1),
        2 | 3 => Codebook(1, 0, 1),
        4 => exp(0),
        5..=8 => Codebook(1, 1, 2),
        9..=14 => exp(1),
        _ => exp(2),
    }
}

/// Table 11: codebook adaptation for `abs_level_minus_1`, keyed by
/// `previousLevelSymbol`.
fn level_codebook(previous_level_symbol: u64) -> Codebook {
    match previous_level_symbol {
        0 => Codebook(2, 0, 2),
        1 => Codebook(1, 0, 1),
        2 => Codebook(2, 0, 1),
        3 => exp(0),
        4..=7 => exp(1),
        _ => exp(2),
    }
}

/// `endOfData(dataSize)`: true when 31 or fewer bits remain in the
/// component's own byte range and every remaining bit is zero.
fn end_of_data(r: &mut BitReader<'_>) -> bool {
    let left = r.bits_left();
    if left > 31 {
        return false;
    }
    if left == 0 {
        return true;
    }
    r.peek(left as u32) == 0
}

/// Decode one color component's `scanned_coefficients()` for a slice —
/// `numBlocks * 64` quantized DCT coefficients, DC-differential then
/// AC run-length, both entropy-coded with adaptively-selected combination
/// codes. `data` is exactly `dataSize` bytes (`coded_size_of_*_data`).
///
/// # Errors
/// [`Error::InvalidData`] on a truncated or malformed codeword; whatever
/// [`Budget::alloc`] returns if the coefficient count is over budget.
pub(crate) fn decode_scanned_coefficients(
    data: &[u8],
    num_blocks: usize,
    budget: &mut Budget,
) -> Result<Vec<i32>> {
    let num_coefficients = num_blocks.saturating_mul(64);
    let mut coeffs = budget.alloc::<i32>(num_coefficients)?;
    if num_blocks == 0 {
        return Ok(coeffs);
    }
    let mut r = BitReader::new(data);

    let first_sym = exp_golomb(&mut r, 5)?;
    let first_dc = symbol_to_signed(first_sym);
    if let Some(slot) = coeffs.get_mut(0) {
        *slot = clamp_i32(first_dc);
    }
    let mut previous_dc_coeff = first_dc;
    let mut previous_dc_diff: i64 = 3;
    let mut n = 1usize;
    while n < num_blocks {
        let codebook = dc_diff_codebook(previous_dc_diff.unsigned_abs());
        let sym = codebook.decode(&mut r)?;
        let mag = symbol_to_signed(sym);
        let dc_coeff_difference = if previous_dc_diff >= 0 { mag } else { -mag };
        let dc_coeff = previous_dc_coeff.saturating_add(dc_coeff_difference);
        if let Some(slot) = coeffs.get_mut(n) {
            *slot = clamp_i32(dc_coeff);
        }
        previous_dc_coeff = dc_coeff;
        previous_dc_diff = dc_coeff_difference;
        n += 1;
    }

    let mut previous_run: u64 = 4;
    let mut previous_level_symbol: u64 = 1;
    while !end_of_data(&mut r) {
        let run = run_codebook(previous_run).decode(&mut r)?;
        let run = usize::try_from(run).unwrap_or(usize::MAX);
        for _ in 0..run {
            if n >= num_coefficients {
                break;
            }
            n += 1;
        }
        previous_run = run as u64;
        let abs_level_minus_1 = level_codebook(previous_level_symbol).decode(&mut r)?;
        let sign = r
            .try_get(1)
            .map_err(|_| Error::InvalidData("prores: coefficient sign truncated"))?;
        let abs_level = abs_level_minus_1.saturating_add(1);
        let abs_level_signed = i64::try_from(abs_level).unwrap_or(i64::MAX);
        let level: i64 = if sign == 1 { -abs_level_signed } else { abs_level_signed };
        if n < num_coefficients {
            if let Some(slot) = coeffs.get_mut(n) {
                *slot = clamp_i32(level);
            }
            n += 1;
        }
        previous_level_symbol = abs_level_minus_1;
    }

    Ok(coeffs)
}

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v > 0 { i32::MAX } else { i32::MIN })
}

/// Decode one slice's `scanned_alpha()` — a raster-scanned run-length-coded
/// array of `numValues` alpha samples (8- or 16-bit).
///
/// # Errors
/// [`Error::InvalidData`] on a truncated or malformed codeword.
pub(crate) fn decode_scanned_alpha(
    data: &[u8],
    num_values: usize,
    sixteen_bit: bool,
    budget: &mut Budget,
) -> Result<Vec<i32>> {
    let mut values = budget.alloc::<i32>(num_values)?;
    if num_values == 0 {
        return Ok(values);
    }
    let mask: i64 = if sixteen_bit { 0xFFFF } else { 0xFF };
    let mut r = BitReader::new(data);
    let mut previous_alpha: i64 = -1;
    let mut n = 0usize;
    let mut guard = 0usize;
    while n < num_values {
        guard += 1;
        if guard > num_values.saturating_add(16) {
            return Err(Error::InvalidData("prores: scanned alpha did not terminate"));
        }
        let (alpha_difference, is_modulo) = decode_alpha_difference(&mut r, sixteen_bit)?;
        let sum = previous_alpha.wrapping_add(alpha_difference);
        let alpha = if is_modulo { sum & mask } else { sum };
        previous_alpha = alpha;
        let run = decode_alpha_run(&mut r)?;
        for _ in 0..run {
            if n >= num_values {
                break;
            }
            if let Some(slot) = values.get_mut(n) {
                *slot = clamp_i32(alpha);
            }
            n += 1;
        }
    }
    Ok(values)
}

/// Table 12: variable-length run-length code for scanned alpha. Run length 1
/// is a single `1` bit; 2..=16 are 5-bit codewords (`0` + the 4-bit binary
/// form of `run - 1`); 17..=2048 escape with 5 zero bits then an 11-bit FLC
/// of `run - 1` (SS7.1.2's own note: the same 5-bit pattern happens to equal
/// `run - 1` in binary, which is what makes the 5 zero bits an unambiguous
/// escape — no real 2..=16 codeword is all zero).
fn decode_alpha_run(r: &mut BitReader<'_>) -> Result<u32> {
    let first = r
        .try_get(1)
        .map_err(|_| Error::InvalidData("prores: alpha run truncated"))?;
    if first == 1 {
        return Ok(1);
    }
    let v = r
        .try_get(4)
        .map_err(|_| Error::InvalidData("prores: alpha run truncated"))?;
    if v != 0 {
        return Ok(v + 1);
    }
    let w = r
        .try_get(11)
        .map_err(|_| Error::InvalidData("prores: alpha run escape truncated"))?;
    Ok(w + 1)
}

/// Tables 13/14: variable-length code for `alpha_difference`, plus the
/// `isModuloAlphaDifference()` flag its own leading bit signals. `mag_bits`
/// is 3 for 8-bit alpha (Table 13) and 6 for 16-bit alpha (Table 14); the
/// escape's fixed-length code is `mag_bits + 5` bits.
fn decode_alpha_difference(r: &mut BitReader<'_>, sixteen_bit: bool) -> Result<(i64, bool)> {
    let escape = r
        .try_get(1)
        .map_err(|_| Error::InvalidData("prores: alpha difference truncated"))?;
    let mag_bits = if sixteen_bit { 6 } else { 3 };
    if escape == 0 {
        let mag_minus_1 = r
            .try_get(mag_bits)
            .map_err(|_| Error::InvalidData("prores: alpha difference truncated"))?;
        let sign = r
            .try_get(1)
            .map_err(|_| Error::InvalidData("prores: alpha difference sign truncated"))?;
        let magnitude = i64::from(mag_minus_1) + 1;
        let diff = if sign == 1 { -magnitude } else { magnitude };
        Ok((diff, false))
    } else {
        let flc_bits = if sixteen_bit { 16 } else { 8 };
        let raw = r
            .try_get(flc_bits)
            .map_err(|_| Error::InvalidData("prores: alpha difference escape truncated"))?;
        Ok((i64::from(raw), true))
    }
}

/// `S(n)` (RDD 36 Table 8) is re-exported through this module for tests that
/// want to build synthetic combo-coded streams without reaching into
/// [`crate::golomb`] directly.
#[cfg(test)]
pub(crate) fn signed_symbol_for_tests(n: i64) -> u64 {
    crate::golomb::signed_to_symbol(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_bitstream::BitWriter;
    use vaco_limits::{Budget, Limits};

    fn encode_combo(w: &mut BitWriter, cb: Codebook, n: u64) {
        let Codebook(last_rice_q, k_rice, k_exp) = cb;
        let rice_max = (u64::from(last_rice_q) + 1) * (1u64 << k_rice);
        if n < rice_max {
            let q = n >> k_rice;
            let r = n & ((1u64 << k_rice) - 1);
            for _ in 0..q {
                w.put(1, 0);
            }
            w.put(1, 1);
            w.put(k_rice, r as u32);
        } else {
            for _ in 0..=last_rice_q {
                w.put(1, 0);
            }
            let inner = n - rice_max;
            let m = inner + (1u64 << k_exp);
            let bits = 64 - m.leading_zeros();
            let q = bits - 1 - k_exp;
            for _ in 0..q {
                w.put(1, 0);
            }
            for i in (0..bits).rev() {
                w.put(1, ((m >> i) & 1) as u32);
            }
        }
    }

    #[test]
    fn all_zero_dc_only_slice_decodes_to_all_zero() {
        // first_dc_coeff = 0 (symbol 0, order-5 exp-golomb of 0 is a single
        // '1' bit), then immediately end-of-data (all zero padding).
        // `end_of_data` requires <= 31 bits remaining, all zero; one trailing
        // zero byte after the 6-bit DC codeword satisfies that with room to
        // spare (`BitWriter::finish` byte-aligns, adding no more than 7 bits).
        let mut w = BitWriter::new();
        encode_combo(&mut w, exp(5), signed_symbol_for_tests(0));
        w.put(8, 0);
        let bytes = w.finish();
        let mut budget = Budget::new(Limits::permissive());
        let coeffs = decode_scanned_coefficients(&bytes, 1, &mut budget).unwrap();
        assert_eq!(coeffs.len(), 64);
        assert!(coeffs.iter().all(|&c| c == 0));
    }

    #[test]
    fn dc_prediction_chain_matches_hand_worked_example() {
        // Two blocks: first_dc_coeff = 5, dc_coeff_difference = -2 for the
        // second (previousDCDiff starts at 3, so codebook is EXP_GOLOMB(3)).
        let mut w = BitWriter::new();
        encode_combo(&mut w, exp(5), signed_symbol_for_tests(5));
        encode_combo(&mut w, exp(3), signed_symbol_for_tests(-2));
        w.put(8, 0); // trailing zero byte for end-of-data
        let bytes = w.finish();
        let mut budget = Budget::new(Limits::permissive());
        let coeffs = decode_scanned_coefficients(&bytes, 2, &mut budget).unwrap();
        // DC coefficients occupy the array's first `numBlocks` entries (SS7.2.1
        // Figure 3: scanned frequency index 0 for every block, before index 1
        // begins) — not one per 64-length block region.
        assert_eq!(coeffs[0], 5);
        assert_eq!(coeffs[1], 3); // 5 + (-2)
    }

    #[test]
    fn run_length_one_and_escape_round_trip() {
        let mut w = BitWriter::new();
        w.put(1, 1); // run = 1
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_alpha_run(&mut r).unwrap(), 1);

        let mut w2 = BitWriter::new();
        w2.put(5, 0); // escape prefix
        w2.put(11, 16); // run - 1 = 16 -> run = 17
        let bytes2 = w2.finish();
        let mut r2 = BitReader::new(&bytes2);
        assert_eq!(decode_alpha_run(&mut r2).unwrap(), 17);
    }

    #[test]
    fn alpha_difference_non_escape_matches_table_13() {
        // +1: '0' then mag_minus_1=000 then sign=0
        let mut w = BitWriter::new();
        w.put(1, 0);
        w.put(3, 0);
        w.put(1, 0);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_alpha_difference(&mut r, false).unwrap(), (1, false));
    }

    #[test]
    fn truncated_alpha_data_errors_not_hangs() {
        let bytes = [0u8; 1];
        let mut budget = Budget::new(Limits::permissive());
        // num_values large relative to the tiny buffer: must error, not loop.
        assert!(decode_scanned_alpha(&bytes, 10_000, false, &mut budget).is_err());
    }
}
