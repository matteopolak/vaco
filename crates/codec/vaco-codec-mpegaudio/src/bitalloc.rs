//! Linear dequantisation shared by Layer I and Layer II, and Layer II's
//! bit-allocation table selection.
//!
//! `Vaco-Spec-Ref: iso-11172-3` §2.4.3.2/§2.4.3.3: read `nb` bits, invert
//! the first (most significant) bit, and treat the result as a fractional
//! two's-complement number whose MSB has weight `-1`; then
//! `s'' = C * (s''' + D)`.

use vaco_format_mpegaudio::Version;

use crate::tables::{
    AllocRow, LAYER2_TABLE_A, LAYER2_TABLE_B, LAYER2_TABLE_C, LAYER2_TABLE_D, LAYER2_TABLE_LSF,
    QUANT_CLASSES,
};

/// Decode one `nb`-bit code (MSB first, as already read from the bitstream)
/// into its fractional two's-complement value in `[-1, 1)`.
pub(crate) fn code_to_fraction(code: u32, nb: u32) -> f32 {
    if nb == 0 {
        return 0.0;
    }
    let half = 1i64 << (nb - 1);
    let full = 1i64 << nb;
    let flipped = i64::from(code) ^ half;
    let signed = if flipped >= half {
        flipped - full
    } else {
        flipped
    };
    signed as f32 / half as f32
}

/// Layer I: `nb` bits allocated directly to one sample, `C = 2^nb/(2^nb-1)`,
/// `D = 2^(1-nb)` — the closed form of the general formula at
/// `nlevels = 2^nb - 1`.
pub(crate) fn layer1_dequant(code: u32, nb: u32) -> f32 {
    if nb == 0 {
        return 0.0;
    }
    let frac = code_to_fraction(code, nb);
    let levels = f64::from((1u32 << nb) - 1);
    let c = (f64::from(1u32 << nb) / levels) as f32;
    let d = 2f32.powi(1 - nb.cast_signed());
    c * (frac + d)
}

/// Layer II ungrouped sample: same formula, `C`/`D` looked up by `nlevels`
/// rather than derived, since `nlevels` is not always `2^nb - 1` (the
/// grouped classes 3/5/9 are not).
pub(crate) fn layer2_dequant_ungrouped(code: u32, bits: u32, nlevels: u32) -> f32 {
    let frac = code_to_fraction(code, bits);
    let (c, d) = quant_constants(nlevels);
    c * (frac + d)
}

/// Layer II grouped triplet: degroup by repeated `% nlevels` / `/ nlevels`
/// (least-significant sample first, `Vaco-Spec-Ref: iso-11172-3` §2.4.3.3's
/// own pseudocode), then requantise each digit with the class's implied
/// sample width before applying the same `C`/`D` formula.
#[allow(
    clippy::integer_division,
    reason = "degrouping a base-`nlevels` codeword is defined by repeated div/mod, not a rounding shortcut"
)]
pub(crate) fn layer2_dequant_grouped(mut combined: u32, nlevels: u32) -> Option<[f32; 3]> {
    if combined >= nlevels.checked_pow(3)? {
        return None;
    }
    let (c, d) = quant_constants(nlevels);
    let sample_bits = nlevels.ilog2() + 1;
    let mut digits = [0u32; 3];
    for slot in &mut digits {
        *slot = combined % nlevels;
        combined /= nlevels;
    }
    let mut out = [0.0f32; 3];
    for (slot, &v) in out.iter_mut().zip(digits.iter()) {
        let frac = code_to_fraction(v, sample_bits);
        *slot = c * (frac + d);
    }
    Some(out)
}

fn quant_constants(nlevels: u32) -> (f32, f32) {
    QUANT_CLASSES
        .iter()
        .find(|cl| cl.nlevels == nlevels)
        .map_or((1.0, 0.0), |cl| (cl.c, cl.d))
}

/// Which of the four (or, at a low sample rate, one) Layer II bit-allocation
/// tables applies, `Vaco-Spec-Ref: iso-11172-3` Annex B Tables 3-B.2a-d
/// headers: keyed on sample rate and per-channel bitrate (total bitrate for
/// mono).
pub(crate) fn layer2_table(
    version: Version,
    sample_rate_hz: u32,
    bitrate_per_channel_kbps: Option<u32>,
) -> &'static [AllocRow] {
    if version.is_low_sample_rate() {
        return LAYER2_TABLE_LSF;
    }
    let low = matches!(bitrate_per_channel_kbps, Some(32 | 48));
    let mid = matches!(bitrate_per_channel_kbps, Some(56 | 64 | 80));
    match sample_rate_hz {
        48000 => {
            if low {
                LAYER2_TABLE_C
            } else {
                LAYER2_TABLE_A
            }
        }
        32000 => {
            if low {
                LAYER2_TABLE_D
            } else if mid {
                LAYER2_TABLE_A
            } else {
                LAYER2_TABLE_B
            }
        }
        _ => {
            if low {
                LAYER2_TABLE_C
            } else if mid {
                LAYER2_TABLE_A
            } else {
                LAYER2_TABLE_B
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer1_two_bit_code_matches_the_spec_worked_range() {
        // nb = 2: half = 2, full = 4; codes 0..3 fold to fractions -1, -0.5,
        // 0, 0.5 in increasing order — the two's-complement property this
        // formula exists to give, checked independently of the final scale.
        let fracs: Vec<f32> = (0..4).map(|c| code_to_fraction(c, 2)).collect();
        assert_eq!(fracs, vec![-1.0, -0.5, 0.0, 0.5]);
    }

    #[test]
    fn layer1_dequant_is_zero_centred_for_a_wide_code() {
        // `code == half` is the fraction-zero code for any width (`flipped =
        // half ^ half = 0`), so a wide allocation's dequantised value there
        // should sit at exactly `C * D`, near zero for a wide `nb`.
        let v = layer1_dequant(1 << 15, 16);
        assert!(v.abs() < 0.01, "{v}");
    }

    #[test]
    fn grouped_codes_cover_each_class_endpoint_and_midpoint() {
        let cases = [
            (3, 0, [-2.0 / 3.0; 3]),
            (3, 13, [0.0; 3]),
            (3, 26, [2.0 / 3.0; 3]),
            (5, 0, [-0.8; 3]),
            (5, 62, [0.0; 3]),
            (5, 124, [0.8; 3]),
            (9, 0, [-8.0 / 9.0; 3]),
            (9, 364, [0.0; 3]),
            (9, 728, [8.0 / 9.0; 3]),
        ];
        for (nlevels, codeword, expected) in cases {
            let decoded = layer2_dequant_grouped(codeword, nlevels);
            assert!(decoded.is_some(), "nlevels={nlevels}, codeword={codeword}");
            assert!(decoded.is_some_and(|actual| {
                actual
                    .iter()
                    .zip(expected)
                    .all(|(&actual, expected)| (actual - expected).abs() < f32::EPSILON)
            }));
        }
    }

    #[test]
    fn grouped_reserved_codewords_are_rejected() {
        for (nlevels, reserved) in [(3, 27), (5, 125), (9, 729)] {
            assert!(layer2_dequant_grouped(reserved, nlevels).is_none());
        }
    }
}
