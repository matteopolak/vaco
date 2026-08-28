//! `spectral_data()` (ISO/IEC 14496-3 subpart 4 Table 4.56) and `pulse_data()`
//! (Table 4.7) — decoding the actual quantized spectral coefficients
//! (`x_quant`), and the pulse-escape adjustment §4.6.3.3 applies directly to
//! them, before inverse quantisation (#445) ever sees them.
//!
//! # The index-to-n-tuple formula
//!
//! A codebook's Huffman decode produces an index, not a spectral value
//! directly; §4.6.3.3 gives the translation as pseudo-C, reproduced here
//! exactly (`mod`/`off` from Table 4.151's `unsigned_cb`/`dim`/`lav` per
//! codebook, in [`crate::spectral_tables::CODEBOOK_INFO`]):
//!
//! ```text
//! if (unsigned) { mod = lav+1; off = 0; } else { mod = 2*lav+1; off = lav; }
//! // dim == 4:
//! w = idx/(mod*mod*mod) - off; idx -= (w+off)*mod*mod*mod;
//! x = idx/(mod*mod) - off;     idx -= (x+off)*mod*mod;
//! y = idx/mod - off;           idx -= (y+off)*mod;
//! z = idx - off;
//! // dim == 2: only y, z, the same way.
//! ```
//!
//! For an unsigned codebook, 0..2 sign bits follow (one per nonzero value in
//! the tuple, `1` = negative). Codebook 11 (`ESC_HCB`) additionally escapes
//! any decoded `y`/`z` of exactly 16: an escape sequence of `N` one-bits, a
//! zero separator, then `N+4` bits, decoding to `2^(N+4) + escape_word`,
//! sign taken from that value's own sign bit as usual.
//!
//! # Section boundaries do not matter here, only codebook runs
//!
//! The specification's own `spectral_data()` loop is per-*section*
//! (`sect_sfb_offset[g][sect_start[g][i]]` to `..sect_end[g][i]]`), but a
//! section is nothing more than "one maximal run of consecutive bands
//! sharing a codebook" — decoding does not depend on *where* the encoder
//! chose to declare a new section within an unbroken run of the same
//! codebook, only on the total coefficient count and the codebook itself.
//! [`crate::section::read_all_groups`] already collapses sections into a
//! flat per-band codebook array; this module re-derives runs from it by
//! merging adjacent equal codebooks, which is equivalent and simpler than
//! carrying section boundaries through an extra layer.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::section::{INTENSITY_HCB, INTENSITY_HCB2, NOISE_HCB, ZERO_HCB};
use crate::spectral_tables::{CODEBOOK_INFO, spectrum_table};
use vaco_codec_vlc::VlcTable;

/// Decode one n-tuple's raw index into its `dim` signed values (no sign
/// bits, no escape — the caller applies both). Mirrors the pseudo-C in the
/// module doc exactly.
#[allow(
    clippy::integer_division,
    reason = "the spec states this decomposition (mod/off, w/x/y/z) as truncating integer division literally, not a precision loss"
)]
fn index_to_tuple(mut idx: i64, unsigned: bool, dim: u8, lav: i64) -> [i64; 4] {
    let (modulus, off) = if unsigned { (lav + 1, 0) } else { (2 * lav + 1, lav) };
    let mut out = [0i64; 4];
    if dim == 4 {
        let m3 = modulus * modulus * modulus;
        let m2 = modulus * modulus;
        let w = idx / m3 - off;
        idx -= (w + off) * m3;
        let x = idx / m2 - off;
        idx -= (x + off) * m2;
        let y = idx / modulus - off;
        idx -= (y + off) * modulus;
        let z = idx - off;
        out[0] = w;
        out[1] = x;
        out[2] = y;
        out[3] = z;
    } else {
        let y = idx / modulus - off;
        idx -= (y + off) * modulus;
        let z = idx - off;
        out[0] = y;
        out[1] = z;
    }
    out
}

/// Read a variable-length escape sequence following a decoded value of
/// exactly 16 in codebook 11: `N` one-bits, a zero separator, `N+4` bits —
/// value `2^(N+4) + escape_word`. §4.6.1.3 bounds `N` such that the whole
/// sequence is under 22 bits, so this loop is bounded without an explicit
/// counter beyond what the bit reader itself enforces.
fn read_escape(r: &mut BitReader<'_>) -> i64 {
    let mut n: u32 = 0;
    while r.get_bit() == 1 {
        n += 1;
        if n > 20 {
            // Not reachable from a conforming stream (§4.6.1.3's own bound);
            // stop rather than loop on adversarial input.
            break;
        }
    }
    let escape_word = r.get_long(n + 4);
    (1i64 << (n + 4)) + escape_word.cast_signed()
}

/// Decode one tuple (dim 2 or 4 signed spectral values) for codebook `hcb`.
fn decode_one_tuple(r: &mut BitReader<'_>, hcb: u8) -> Result<Vec<i32>> {
    let info = CODEBOOK_INFO.get(usize::from(hcb)).copied().ok_or(
        Error::InvalidData("vaco-codec-aac: spectral_data codebook out of range"),
    )?;
    let table = spectrum_table(hcb).ok_or(Error::InvalidData(
        "vaco-codec-aac: spectral_data codebook has no Huffman table",
    ))?;
    let vlc = VlcTable::new(table);
    let idx = vlc.decode(r).ok_or(Error::InvalidData(
        "vaco-codec-aac: spectral Huffman codeword matches no entry",
    ))?;
    let mut values = index_to_tuple(i64::from(idx), info.unsigned, info.dim, i64::from(info.lav));
    let dim = usize::from(info.dim);
    let is_esc = hcb == 11;

    if info.unsigned {
        // §4.6.3.3: "the ordering of data elements is Huffman codeword
        // followed by 0 to 2 sign bits followed by 0 to 2 escape
        // sequences" — sign bits for every nonzero value first, *then*
        // escape sequences for every value that hit 16, in the same order.
        // Interleaving sign-then-escape per value (reading value 0's
        // escape sequence before value 1's sign bit) reads the bitstream in
        // the wrong order the moment a dim-2 tuple has two
        // escape-triggering values — exactly what a real ffmpeg-encoded
        // stereo fixture's codebook-11 sections hit, caught by this crate's
        // own bit-exact-consumption verification, not by a unit test.
        let mut negative = [false; 4];
        for (v, neg) in values.iter().zip(negative.iter_mut()).take(dim) {
            if *v != 0 {
                *neg = r.get_bit() == 1;
            }
        }
        if is_esc {
            for v in values.iter_mut().take(dim) {
                if *v == 16 {
                    *v = read_escape(r);
                }
            }
        }
        for (v, &neg) in values.iter_mut().zip(negative.iter()).take(dim) {
            if neg {
                *v = -*v;
            }
        }
    }

    Ok(values.into_iter().take(dim).map(|v| v as i32).collect())
}

/// One maximal run of consecutive bands (within a window group) sharing a
/// real spectral codebook (1..=11) — see the module doc for why section
/// boundaries themselves are irrelevant to decoding. `start`/`coefficient_count`
/// are absolute positions within the group's full coefficient array (the
/// `sum(widths)`-length array `read_one_group` returns), so a caller (pulse
/// application) can index directly by frequency line rather than needing its
/// own band-offset bookkeeping.
struct Run {
    hcb: u8,
    start: u32,
    coefficient_count: u32,
}

/// Merge a window group's per-band codebook assignment into codebook runs,
/// converting each run's band span into an absolute-position range via
/// `widths` (already scaled by `window_group_length` for a short-window
/// group) — `start`/`coefficient_count` index into the
/// `sum(widths)`-length full coefficient array, not into `sfb_cb` itself.
fn runs_for_group(sfb_cb: &[u8], widths: &[u32]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut i = 0;
    let mut pos = 0u32;
    while let Some(&hcb) = sfb_cb.get(i) {
        let start = pos;
        let mut j = i;
        while sfb_cb.get(j) == Some(&hcb) {
            pos += widths.get(j).copied().unwrap_or(0);
            j += 1;
        }
        if !matches!(hcb, ZERO_HCB | NOISE_HCB | INTENSITY_HCB | INTENSITY_HCB2) {
            runs.push(Run {
                hcb,
                start,
                coefficient_count: pos - start,
            });
        }
        i = j;
    }
    runs
}

/// Decode `spectral_data()` for one window group into a full,
/// zero-filled-where-absent `x_quant` array of length `widths.iter().sum()`
/// — one raw (not yet inverse-quantised) integer per frequency line, in
/// ascending order. A `ZERO_HCB`/noise/intensity band's lines are `0`,
/// matching §4.6.3.3: "spectral information for sections ... coded with the
/// zero codebook is not sent as this spectral information is zero." A full
/// array (rather than only the transmitted, non-zero-codebook coefficients)
/// is what [`crate::pulse::apply`] needs to index by absolute frequency
/// line, and is the natural `x_quant[]` shape #445 will want as input
/// regardless.
///
/// # Errors
///
/// [`Error::InvalidData`] if a run's coefficient count is not a multiple of
/// its codebook's dimension (2 or 4) — this can only happen if `widths` (the
/// caller's `swb_offset` slice) disagrees with what the codebook run
/// actually spans, since the specification's own `swb_offset` tables are
/// constructed so that every legally-sectioned run's length divides evenly.
/// Also whatever [`decode_one_tuple`] returns for a truncated or corrupt
/// codeword.
pub(crate) fn read_one_group(
    r: &mut BitReader<'_>,
    sfb_cb: &[u8],
    widths: &[u32],
) -> Result<Vec<i32>> {
    let total: u32 = widths.iter().sum();
    let mut out = vec![0i32; total as usize];
    for run in runs_for_group(sfb_cb, widths) {
        let Some(info) = CODEBOOK_INFO.get(usize::from(run.hcb)).copied() else {
            continue;
        };
        let dim = u32::from(info.dim);
        if dim == 0 || run.coefficient_count % dim != 0 {
            return Err(Error::InvalidData(
                "vaco-codec-aac: spectral_data run length is not a multiple of its codebook's dimension",
            ));
        }
        let mut pos = run.start as usize;
        #[allow(
            clippy::integer_division,
            reason = "dim is checked non-zero and evenly-dividing coefficient_count just above"
        )]
        let tuple_count = run.coefficient_count / dim;
        for _ in 0..tuple_count {
            for v in decode_one_tuple(r, run.hcb)? {
                if let Some(slot) = out.get_mut(pos) {
                    *slot = v;
                }
                pos += 1;
            }
        }
    }
    Ok(out)
}

/// Regression test data lives alongside the code it guards; see
/// `tests::sign_bits_are_grouped_before_both_escape_sequences_not_interleaved`
/// for the bug this shape caught.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::{index_to_tuple, read_one_group};
    use crate::spectral_tables::SPECTRUM_HCB_7;
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn signed_quad_formula_matches_the_spec_pseudocode_for_hcb1() {
        // hcb1: dim=4, lav=1, signed -> mod=3, off=1. idx=0 should be
        // (-1,-1,-1,-1); idx = mod^3*2 + mod^2*2 + mod*2 + 2 = 80 (the max
        // index, 81 entries 0..80) should be (1,1,1,1).
        assert_eq!(index_to_tuple(0, false, 4, 1), [-1, -1, -1, -1]);
        assert_eq!(index_to_tuple(80, false, 4, 1), [1, 1, 1, 1]);
    }

    #[test]
    fn unsigned_pair_formula_matches_the_spec_pseudocode_for_hcb7() {
        // hcb7: dim=2, lav=7, unsigned -> mod=8, off=0. idx=0 -> (0,0).
        // idx = 8*7+7=63 (max, 64 entries) -> (7,7).
        assert_eq!(index_to_tuple(0, true, 2, 7)[..2], [0, 0]);
        assert_eq!(index_to_tuple(63, true, 2, 7)[..2], [7, 7]);
    }

    #[test]
    fn a_real_hcb7_codeword_decodes_with_its_sign_bits() {
        // Find the codeword for index (y=3,z=0) -> idx = 3*8+0 = 24.
        let entry = SPECTRUM_HCB_7.iter().find(|e| e.symbol == 24).unwrap();
        let mut w = BitWriter::new();
        w.put(u32::from(entry.len), entry.code);
        w.put(1, 1); // sign bit for y (nonzero) -> negative
        // z == 0, no sign bit for it.
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let sfb_cb = [7u8];
        let widths = [2u32];
        let values = read_one_group(&mut r, &sfb_cb, &widths).unwrap();
        assert_eq!(values, vec![-3, 0]);
    }

    #[test]
    fn zero_and_noise_and_intensity_bands_consume_no_spectral_bits() {
        let bytes: Vec<u8> = vec![];
        let mut r = BitReader::new(&bytes);
        let sfb_cb = [0u8, 13, 14, 15];
        let widths = [4u32, 4, 4, 4];
        let values = read_one_group(&mut r, &sfb_cb, &widths).unwrap();
        // Full-length, zero-filled array — nothing was transmitted for any
        // of these four bands, but the array still covers all 16 lines.
        assert_eq!(values, vec![0i32; 16]);
    }

    #[test]
    fn a_run_length_not_divisible_by_the_codebooks_dimension_is_rejected() {
        let bytes: Vec<u8> = vec![0u8; 8];
        let mut r = BitReader::new(&bytes);
        let sfb_cb = [1u8]; // dim 4
        let widths = [3u32]; // not a multiple of 4
        assert!(read_one_group(&mut r, &sfb_cb, &widths).is_err());
    }

    #[test]
    fn adjacent_bands_with_the_same_codebook_decode_as_one_continuous_run() {
        // Two bands, both codebook 5 (dim 2, signed, lav 4 -> mod 9), total
        // width 4 -> 2 tuples decoded back to back with no boundary artefact.
        use crate::spectral_tables::SPECTRUM_HCB_5;
        let e0 = SPECTRUM_HCB_5.iter().find(|e| e.symbol == 0).unwrap(); // (-4,-4)
        let e1 = SPECTRUM_HCB_5
            .iter()
            .find(|e| e.symbol == 9 * 4 + 4)
            .unwrap(); // idx=40 -> y=0,z=0? recompute: mod=9,off=4; idx=9*4+4=40 -> y=40/9-4=0,z=40-9*4-4=0
        let mut w = BitWriter::new();
        w.put(u32::from(e0.len), e0.code);
        w.put(u32::from(e1.len), e1.code);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let sfb_cb = [5u8, 5u8];
        let widths = [2u32, 2u32];
        let values = read_one_group(&mut r, &sfb_cb, &widths).unwrap();
        assert_eq!(values, vec![-4, -4, 0, 0]);
    }

    /// The bug a hand-built vector could not have caught, but a real
    /// ffmpeg-encoded stereo fixture's bit-exact-consumption check did:
    /// §4.6.3.3 states the ESC codebook's data order as "Huffman codeword
    /// followed by 0 to 2 sign bits followed by 0 to 2 escape sequences" —
    /// **both** sign bits before **either** escape sequence, not
    /// interleaved sign-then-escape per value. A tuple whose *both* values
    /// (y and z) hit the escape flag (16) is exactly the case that
    /// distinguishes the two orderings; anything with at most one escaping
    /// value reads identically either way.
    #[test]
    fn sign_bits_are_grouped_before_both_escape_sequences_not_interleaved() {
        use crate::spectral_tables::SPECTRUM_HCB_11;
        // y=16, z=16 -> idx = 16*17+16 = 288, the ESC codebook's top index.
        let entry = SPECTRUM_HCB_11.iter().find(|e| e.symbol == 288).unwrap();
        let mut w = BitWriter::new();
        w.put(u32::from(entry.len), entry.code);
        // Correct order: sign_y, sign_z, escape_y, escape_z.
        w.put(1, 1); // sign_y: negative
        w.put(1, 0); // sign_z: positive
        // escape_y: N=0 ones, 1 zero separator, 4-bit word = 5 -> 16+5=21
        w.put(1, 0);
        w.put(4, 5);
        // escape_z: N=1 one, 1 zero separator, 5-bit word = 3 -> 32+3=35
        w.put(1, 1);
        w.put(1, 0);
        w.put(5, 3);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let sfb_cb = [11u8];
        let widths = [2u32];
        let values = read_one_group(&mut r, &sfb_cb, &widths).unwrap();
        assert_eq!(values, vec![-21, 35]);
    }
}
