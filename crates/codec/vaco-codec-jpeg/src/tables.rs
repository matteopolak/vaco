//! The constant tables ITU-T T.81 either mandates or recommends as defaults.
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209` (registered in `provenance/sources.toml`,
//! table rows in `provenance/vaco-codec-jpeg.toml`).

/// The zig-zag scan order (Annex A, Figure A.6): `ZIGZAG[k]` is the natural
/// (row-major) index of the coefficient a decoder stores at scan position
/// `k`. Every block-structured JPEG stream uses this exact ordering; it is
/// not a per-encoder choice.
pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, //
    17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, //
    27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, //
    29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, //
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// Table K.1: the recommended luminance quantization table, in natural
/// (row-major) order. An encoder's "quality" setting scales this table; a
/// decoder uses whatever `DQT` actually transmits, and only falls back to
/// this when asked to reproduce the reference default explicitly.
pub(crate) const STD_LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, //
    12, 12, 14, 19, 26, 58, 60, 55, //
    14, 13, 16, 24, 40, 57, 69, 56, //
    14, 17, 22, 29, 51, 87, 80, 62, //
    18, 22, 37, 56, 68, 109, 103, 77, //
    24, 35, 55, 64, 81, 104, 113, 92, //
    49, 64, 78, 87, 103, 121, 120, 101, //
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Table K.2: the recommended chrominance quantization table, natural order.
pub(crate) const STD_CHROMA_QUANT: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, //
    18, 21, 26, 66, 99, 99, 99, 99, //
    24, 26, 56, 99, 99, 99, 99, 99, //
    47, 66, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// One `BITS`/`HUFFVAL` pair (Annex C's own naming): `counts[l - 1]` is the
/// number of codes of length `l`, and `values` lists the symbols in
/// shortest-code-first order. This is the on-the-wire shape of a `DHT`
/// segment and of each Annex K default table.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HuffSpec {
    pub counts: [u8; 16],
    pub values: &'static [u8],
}

/// Table K.3's `HUFFVAL`. Twelve entries, below the size this project
/// registers provenance for on its own — kept as a named array anyway, for
/// the same reason [`STD_AC_LUMA_VALUES`] is one, so every default table
/// has the same shape.
const STD_DC_LUMA_VALUES: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.4's `HUFFVAL`.
const STD_DC_CHROMA_VALUES: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.5's `HUFFVAL`: 162 entries, the full AC luminance symbol set.
const STD_AC_LUMA_VALUES: &[u8] = &[
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
    0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
    0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
    0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
    0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
    0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
    0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
    0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
    0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
    0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
    0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// Table K.6's `HUFFVAL`: 162 entries, the full AC chrominance symbol set.
const STD_AC_CHROMA_VALUES: &[u8] = &[
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
    0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
    0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
    0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
    0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
    0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
    0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
    0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
    0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
    0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// Table K.3: the recommended DC luminance Huffman table.
pub(crate) const STD_DC_LUMA: HuffSpec = HuffSpec {
    counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    values: STD_DC_LUMA_VALUES,
};

/// Table K.4: the recommended DC chrominance Huffman table.
pub(crate) const STD_DC_CHROMA: HuffSpec = HuffSpec {
    counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    values: STD_DC_CHROMA_VALUES,
};

/// Table K.5: the recommended AC luminance Huffman table.
pub(crate) const STD_AC_LUMA: HuffSpec = HuffSpec {
    counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    values: STD_AC_LUMA_VALUES,
};

/// Table K.6: the recommended AC chrominance Huffman table.
pub(crate) const STD_AC_CHROMA: HuffSpec = HuffSpec {
    counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    values: STD_AC_CHROMA_VALUES,
};

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test code over a fixed 64-entry array"
)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_is_a_permutation_of_0_to_63() {
        let mut seen = [false; 64];
        for &idx in &ZIGZAG {
            assert!(idx < 64);
            assert!(!seen[idx], "index {idx} repeated");
            seen[idx] = true;
        }
    }

    fn check_spec(spec: &HuffSpec) {
        let total: usize = spec.counts.iter().map(|&c| usize::from(c)).sum();
        assert_eq!(total, spec.values.len());
        // Sixteen is the longest code Annex C's canonical construction ever
        // assigns, so a well-formed default table never needs a 17th count.
        assert_eq!(spec.counts.len(), 16);
    }

    #[test]
    fn every_standard_huffman_spec_is_internally_consistent() {
        check_spec(&STD_DC_LUMA);
        check_spec(&STD_DC_CHROMA);
        check_spec(&STD_AC_LUMA);
        check_spec(&STD_AC_CHROMA);
        assert_eq!(STD_AC_LUMA.values.len(), 162);
        assert_eq!(STD_AC_CHROMA.values.len(), 162);
    }
}
