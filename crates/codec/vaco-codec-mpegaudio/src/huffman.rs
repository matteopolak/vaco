//! Layer III Huffman decoding: table lookup and the two `count1` quad tables.

use vaco_bitstream::BitReader;

use crate::huffman_data::{HUFF_QUAD_A, HUFF_QUAD_B, HUFFMAN_LINBITS, HuffEntry, huffman_table};

/// Find the entry in `table` whose code matches the next bits of `r`,
/// consuming exactly its length. `None` if nothing matches within the
/// stream that remains — a malformed or truncated bitstream.
fn lookup(r: &mut BitReader<'_>, table: &[HuffEntry]) -> Option<(u8, u8)> {
    for entry in table {
        if r.peek(u32::from(entry.len)) == u32::from(entry.code) {
            r.skip(u32::from(entry.len));
            return Some((entry.x, entry.y));
        }
    }
    None
}

/// One decoded `(x, y)` pair from a "big values" region, already carrying
/// its escape extension and sign.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BigValue {
    pub x: i32,
    pub y: i32,
}

/// Decode one `(x, y)` pair from `table_select` (0..=31). Table 0 is the
/// all-zero table: it consumes no bits and always yields `(0, 0)`.
pub(crate) fn decode_big_value(r: &mut BitReader<'_>, table_select: u8) -> Option<BigValue> {
    if table_select == 0 {
        return Some(BigValue { x: 0, y: 0 });
    }
    let table = huffman_table(table_select)?;
    let (x, y) = lookup(r, table)?;
    let linbits = u32::from(*HUFFMAN_LINBITS.get(usize::from(table_select))?);

    let mut xi = i32::from(x);
    if x == 15 && linbits > 0 {
        xi += r.get(linbits).cast_signed();
    }
    if xi != 0 && r.get(1) == 1 {
        xi = -xi;
    }
    let mut yi = i32::from(y);
    if y == 15 && linbits > 0 {
        yi += r.get(linbits).cast_signed();
    }
    if yi != 0 && r.get(1) == 1 {
        yi = -yi;
    }
    Some(BigValue { x: xi, y: yi })
}

/// Decode one quadruple `(v, w, x, y)` from the `count1` region.
pub(crate) fn decode_count1(r: &mut BitReader<'_>, table_select: u8) -> Option<[i32; 4]> {
    let table = if table_select == 0 { HUFF_QUAD_A } else { HUFF_QUAD_B };
    let (packed, _) = lookup(r, table)?;
    let mut out = [0i32; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        let bit = (packed >> (3 - i)) & 1;
        if bit == 1 {
            *slot = if r.get(1) == 1 { -1 } else { 1 };
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// Every non-empty table (0 is the special all-zero case) must be a
    /// complete, prefix-free code: an unambiguous transcription check that
    /// does not depend on knowing any codeword in advance.
    #[test]
    fn every_table_is_a_complete_prefix_free_code() {
        for idx in 1u8..32 {
            let Some(table) = huffman_table(idx) else {
                continue;
            };
            check_complete_prefix_free(table, idx);
        }
        check_complete_prefix_free(HUFF_QUAD_A, 100);
        check_complete_prefix_free(HUFF_QUAD_B, 101);
    }

    fn check_complete_prefix_free(table: &[HuffEntry], id: u8) {
        let mut kraft = 0.0f64;
        for e in table {
            kraft += 2f64.powi(-i32::from(e.len));
        }
        assert!(
            (kraft - 1.0).abs() < 1e-9,
            "table {id}: Kraft sum {kraft}, expected 1.0"
        );
        for (i, a) in table.iter().enumerate() {
            for b in &table[i + 1..] {
                let (short, long) = if a.len <= b.len { (a, b) } else { (b, a) };
                let prefix = u32::from(long.code) >> (long.len - short.len);
                assert_ne!(
                    prefix,
                    u32::from(short.code),
                    "table {id}: {a:?} is a prefix of {b:?} or vice versa"
                );
            }
        }
    }

    #[test]
    fn table_zero_is_free() {
        let mut r = BitReader::new(&[0xFFu8]);
        let v = decode_big_value(&mut r, 0).expect("table 0 always decodes");
        assert_eq!((v.x, v.y), (0, 0));
        assert_eq!(r.bit_pos(), 0);
    }
}
