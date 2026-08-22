//! Exp-Golomb tests, checked against ITU-T H.264 §9.1 derived independently from
//! the specification text.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{BitReader, BitWriter, BitstreamError, GolombRead};

/// The codeword for `code_num`, built straight from the §9.1 definition.
///
/// `leadingZeroBits` is the largest `n` with `2^n - 1 <= code_num`; the codeword
/// is `leadingZeroBits` zeros, a one, then `code_num + 1 - 2^leadingZeroBits` in
/// `leadingZeroBits` bits.
fn codeword(code_num: u32) -> String {
    let mut lz = 0u32;
    while (1u64 << (lz + 1)) - 1 <= u64::from(code_num) {
        lz += 1;
    }
    let suffix = u64::from(code_num) + 1 - (1u64 << lz);
    let mut s = "0".repeat(lz as usize);
    s.push('1');
    for i in (0..lz).rev() {
        s.push(if (suffix >> i) & 1 == 1 { '1' } else { '0' });
    }
    s
}

/// Pack a bit string into bytes, MSB first, zero-padded to a byte boundary.
fn pack(bits: &str) -> Vec<u8> {
    let mut w = BitWriter::new();
    for c in bits.chars() {
        w.put(1, u32::from(c == '1'));
    }
    w.finish()
}

#[test]
fn the_first_codewords_match_the_specification() {
    // Spot-check the definition itself against the values §9.1 tabulates.
    assert_eq!(codeword(0), "1");
    assert_eq!(codeword(1), "010");
    assert_eq!(codeword(2), "011");
    assert_eq!(codeword(3), "00100");
    assert_eq!(codeword(4), "00101");
    assert_eq!(codeword(5), "00110");
    assert_eq!(codeword(6), "00111");
    assert_eq!(codeword(7), "0001000");
    assert_eq!(codeword(8), "0001001");

    for k in 0..64u32 {
        let bytes = pack(&codeword(k));
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue(), k, "code_num {k}");
        assert!(!r.overrun(), "code_num {k}");
    }
}

#[test]
fn se_maps_code_numbers_as_the_specification_says() {
    // §9.1.1: k=0 -> 0, k=1 -> 1, k=2 -> -1, k=3 -> 2, k=4 -> -2, ...
    let expected = [0i32, 1, -1, 2, -2, 3, -3, 4, -4, 5, -5];
    for (k, &want) in expected.iter().enumerate() {
        let bytes = pack(&codeword(k as u32));
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.se(), want, "code_num {k}");
    }
}

#[test]
fn concatenated_codewords_decode_in_sequence() {
    let values = [0u32, 5, 1, 300, 2, 65_535, 7, 1_048_575];
    let bits: String = values.iter().map(|&v| codeword(v)).collect();
    let bytes = pack(&bits);
    let mut r = BitReader::new(&bytes);
    for &v in &values {
        assert_eq!(r.ue(), v);
    }
    assert!(!r.overrun());
}

#[test]
fn an_impossible_prefix_rejects_instead_of_looping() {
    // 33 zero bits then a one: longer than `ue` can express.
    let bytes = pack(&format!("{}1", "0".repeat(33)));
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue(), 0);
    assert!(r.overrun());
    assert!(r.check().is_err());

    // An all-zero buffer must terminate, not spin.
    let mut r = BitReader::new(&[0u8; 4096]);
    for _ in 0..16 {
        assert_eq!(r.ue(), 0);
    }
    assert!(r.overrun());

    // The 64-bit form has the same property one octave up.
    let bytes = pack(&format!("{}1", "0".repeat(64)));
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_long(), 0);
    assert!(r.overrun());
}

#[test]
fn ue_long_reads_wide_prefixes() {
    for lz in [0u32, 1, 15, 31, 32, 40, 63] {
        // The smallest value with this prefix length: 2^lz - 1.
        let value = (1u64 << lz) - 1;
        let mut bits = "0".repeat(lz as usize);
        bits.push('1');
        bits.push_str(&"0".repeat(lz as usize));
        let bytes = pack(&bits);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue_long(), value, "lz {lz}");
        assert!(!r.overrun(), "lz {lz}");
    }
}

#[test]
fn ue_max_reports_the_reason_it_failed() {
    let bytes = pack(&codeword(100));
    let mut r = BitReader::new(&bytes);
    assert_eq!(
        r.ue_max(50),
        Err(BitstreamError::ValueTooLarge {
            value: 100,
            max: 50
        })
    );

    let bytes = pack(&codeword(10));
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_max(50), Ok(10));

    let mut r = BitReader::new(&[]);
    assert!(r.ue_max(50).is_err());
}

#[test]
fn se_range_checks_both_ends() {
    let bytes = pack(&codeword(19)); // -> +10
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.se_range(-100, 100), Ok(10));

    let mut r = BitReader::new(&bytes);
    assert!(r.se_range(-100, 5).is_err());

    let bytes = pack(&codeword(20)); // -> -10
    let mut r = BitReader::new(&bytes);
    assert!(r.se_range(-5, 100).is_err());
}

#[test]
fn order_k_generalises_order_zero() {
    for v in 0..200u32 {
        let bytes = pack(&codeword(v));
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue_golomb_k(0), v);
    }

    // Order-k: value = (2^lz - 1) * 2^k + read(lz + k).
    // k = 2, prefix "1" (lz = 0), suffix "10" -> 0 * 4 + 2 = 2.
    let bytes = pack("110");
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_golomb_k(2), 2);

    // k = 1, prefix "01" (lz = 1), suffix "11" -> 1 * 2 + 3 = 5.
    let bytes = pack("0111");
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_golomb_k(1), 5);

    // A prefix that would overflow the suffix width rejects.
    let bytes = pack(&format!("{}1{}", "0".repeat(30), "1".repeat(40)));
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_golomb_k(8), 0);
    assert!(r.overrun());
}

#[test]
fn truncation_in_the_middle_of_a_codeword_is_clean() {
    // code_num 1000 is a 19-bit codeword. Keep only its first two bytes: the
    // prefix survives, the suffix does not.
    let full = pack(&codeword(1000));
    assert_eq!(full.len(), 3);
    let truncated = &full[..2];
    let mut r = BitReader::new(truncated);
    let _ = r.ue();
    assert!(r.overrun());
    assert!(r.check().is_err());

    // Zero padding to a byte boundary can complete a codeword, and that is not
    // an overrun — the bits really are there.
    let bytes = pack("0001");
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue(), 7);
    assert!(!r.overrun());
}
