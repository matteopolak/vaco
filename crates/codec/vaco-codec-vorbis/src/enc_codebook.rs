//! Encode-side codebook construction (spec section 3.2.1), the mirror of
//! [`crate::codebook`]'s decode side.
//!
//! Every codebook this encoder emits uses one deliberately restricted shape:
//! **flat and ordered** — every entry has exactly the same codeword length
//! `code_bits = ceil(log2(entries))` (or the spec's single-entry special
//! case when `entries == 1`), written as one run via the setup header's
//! `ordered` length encoding (spec 3.2.1). This is the "fixed low-complexity"
//! choice this batch's brief calls out explicitly: it costs more bits per
//! symbol than a probability-matched Huffman code would, but it is trivial
//! to get right (canonical-Huffman assignment for a uniform-length list is
//! just binary counting, so the encoder never has to reproduce
//! [`crate::codebook`]'s leftmost-first-fit search) and, being always
//! complete and prefix-free by construction, can never trip the decoder's
//! own Kraft-sum check. Real per-symbol Huffman lengths matched to a
//! measured residue/floor distribution are the natural next step past this
//! batch — noted here rather than attempted, per the brief's own "even
//! simple" framing for a first working encode.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 3.2.1 and 9.2.2/9.2.3`

use crate::bitreader::{BitWriterLsb, ilog};

const CODEBOOK_SYNC: u32 = 0x0056_4342;

/// `ceil(log2(n))` for `n >= 1`: the codeword width a flat code over `n`
/// entries needs, `0` only for `n == 1` (handled by the caller as the
/// spec's single-entry special case, codeword length forced to `1`).
#[must_use]
pub(crate) fn flat_code_bits(entries: u32) -> u32 {
    if entries <= 1 {
        return 0;
    }
    let mut bits = 0u32;
    while (1u32 << bits) < entries {
        bits = bits.saturating_add(1);
    }
    bits
}

/// Write a flat/ordered codebook with no value mapping (lookup type 0) —
/// used for floor1's per-class subclass books and the residue classbook,
/// both of which are read in scalar-only context (spec 7.2.3, 8.6.1).
///
/// `entries` must be `1` or an exact power of two (every call site here
/// chooses such a value; see the module doc for why that keeps the flat
/// scheme's Kraft sum exact).
pub(crate) fn write_scalar_codebook(w: &mut BitWriterLsb, entries: u32) {
    write_flat_lengths(w, entries);
    w.put(0, 4); // lookup type 0: no value mapping.
}

/// Write a flat/ordered, one-dimensional VQ codebook (lookup type 1) whose
/// entry `i` decodes to the scalar value `min_value + i * delta_value` —
/// a uniform scalar quantiser dressed as the spec's lattice lookup, chosen
/// because `lookup1_values(entries, 1) == entries` makes the multiplicand
/// list exactly the identity sequence `0..entries`, needing no lattice
/// arithmetic at all.
///
/// `value_bits` must be wide enough to hold `entries - 1` (the caller picks
/// `entries` as a power of two, so `value_bits = flat_code_bits(entries)`
/// is always exact).
pub(crate) fn write_scalar_vq_codebook(
    w: &mut BitWriterLsb,
    entries: u32,
    min_value: f32,
    delta_value: f32,
    value_bits: u32,
) {
    write_flat_lengths(w, entries);
    w.put(1, 4); // lookup type 1: lattice VQ.
    w.put(float32_pack(min_value), 32);
    w.put(float32_pack(delta_value), 32);
    w.put(value_bits.saturating_sub(1), 4);
    w.put_bool(false); // sequence_p: each entry's one value stands alone.
    for i in 0..entries {
        w.put(i, value_bits);
    }
}

/// The sync pattern, dimensions, entry count and one-run ordered length
/// list shared by every codebook this module writes.
fn write_flat_lengths(w: &mut BitWriterLsb, entries: u32) {
    w.put(CODEBOOK_SYNC, 24);
    w.put(1, 16); // dimensions: every codebook here is one-dimensional.
    w.put(entries, 24);
    w.put_bool(true); // ordered.
    let code_bits = if entries <= 1 {
        1
    } else {
        flat_code_bits(entries)
    };
    w.put(code_bits.saturating_sub(1), 5); // start length, stored as length-1.
    // One run: every entry shares the same length, so the ordered encoding
    // needs exactly one (length, count) pair covering all of them.
    let bits = ilog(i64::from(entries));
    w.put(entries, bits);
}

/// `float32_pack`: the exact inverse of [`crate::codebook`]'s
/// `float32_unpack` (spec 9.2.2), for the lattice lookup's `min_value` and
/// `delta_value` fields. Only finite inputs within the format's dynamic
/// range are ever passed here (this encoder's own chosen quantiser bounds),
/// so there is no NaN/infinity case to round-trip.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "mantissa is masked to 21 bits and exponent clamped to the format's own field width before the cast"
)]
fn float32_pack(x: f32) -> u32 {
    let sign = x.is_sign_negative();
    let mag = f64::from(x.abs());
    if mag == 0.0 {
        return 0;
    }
    // Find the smallest exponent (spec bias 788) such that the mantissa
    // fits in 21 bits, by direct search from the value's own binary
    // exponent rather than repeated division — `mag` is always a small,
    // finite, encoder-chosen constant, so a fixed 64-iteration search is
    // both exact and cheap.
    let mut exponent: i32 = 788;
    let mut mantissa = mag;
    while mantissa >= 2_097_152.0 && exponent < 788 + 63 {
        mantissa /= 2.0;
        exponent = exponent.saturating_add(1);
    }
    while mantissa < 1_048_576.0 && exponent > 0 {
        mantissa *= 2.0;
        exponent = exponent.saturating_sub(1);
    }
    let mantissa = (mantissa.round() as u32) & 0x001f_ffff;
    let exponent = exponent.clamp(0, 2047).cast_unsigned();
    let sign_bit = if sign { 0x8000_0000u32 } else { 0 };
    sign_bit | (exponent << 21) | mantissa
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::bitreader::BitReaderLsb;
    use crate::codebook::Codebook;
    use vaco_limits::{Budget, Limits};

    fn parse(bytes: &[u8]) -> Codebook {
        let mut r = BitReaderLsb::new(bytes);
        let mut budget = Budget::new(Limits::permissive());
        Codebook::parse(&mut r, &mut budget).unwrap()
    }

    #[test]
    fn flat_code_bits_matches_powers_of_two() {
        assert_eq!(flat_code_bits(1), 0);
        assert_eq!(flat_code_bits(2), 1);
        assert_eq!(flat_code_bits(256), 8);
        assert_eq!(flat_code_bits(128), 7);
    }

    #[test]
    fn scalar_codebook_round_trips_every_entry() {
        for &entries in &[1u32, 2, 8, 256] {
            let mut w = BitWriterLsb::new();
            write_scalar_codebook(&mut w, entries);
            let bytes = w.finish();
            let book = parse(&bytes);
            assert!(!book.has_lookup());

            // Every entry must be reachable by decoding the exact codeword
            // this module's own `write_flat_lengths` scheme assigns it —
            // the flat code's codeword for entry `i` is `i` itself, in
            // `code_bits` bits, MSb (root decision) first.
            let code_bits = if entries <= 1 {
                1
            } else {
                flat_code_bits(entries)
            };
            for entry in 0..entries {
                let mut bw = BitWriterLsb::new();
                for bit_index in (0..code_bits).rev() {
                    bw.put_tree_bit((entry >> bit_index) & 1);
                }
                let bytes = bw.finish();
                let mut r = BitReaderLsb::new(&bytes);
                assert_eq!(book.decode_scalar(&mut r), Some(entry));
            }
        }
    }

    #[test]
    fn scalar_vq_codebook_decodes_the_identity_quantiser() {
        let entries = 16u32;
        let value_bits = flat_code_bits(entries);
        let mut w = BitWriterLsb::new();
        write_scalar_vq_codebook(&mut w, entries, -2.0, 0.25, value_bits);
        let bytes = w.finish();
        let book = parse(&bytes);
        assert!(book.has_lookup());

        for entry in 0..entries {
            let mut bw = BitWriterLsb::new();
            for bit_index in (0..value_bits).rev() {
                bw.put_tree_bit((entry >> bit_index) & 1);
            }
            let packed = bw.finish();
            let mut r = BitReaderLsb::new(&packed);
            let v = book.decode_vector(&mut r).unwrap();
            let expected = -2.0 + f32::from(entry as u16) * 0.25;
            assert!(
                (v[0] - expected).abs() < 1e-4,
                "entry {entry}: {v:?} vs {expected}"
            );
        }
    }

    #[test]
    fn float32_pack_unpack_round_trips_small_values() {
        for &v in &[0.0f32, 1.0, -1.0, 0.25, -4.0, 3.5, 127.0] {
            let packed = float32_pack(v);
            // `float32_unpack` lives in `crate::codebook` as a private fn;
            // round-trip it indirectly through a real lookup-1 codebook
            // with a single entry instead of reaching into that module.
            let mut w = BitWriterLsb::new();
            write_scalar_vq_codebook(&mut w, 1, v, 1.0, 1);
            let bytes = w.finish();
            let book = parse(&bytes);
            let mut r = BitReaderLsb::new(&[0u8]);
            let out = book.decode_vector(&mut r).unwrap()[0];
            assert!(
                (out - v).abs() < 1e-3,
                "{v} packed as {packed:#x} decoded to {out}"
            );
        }
    }
}
