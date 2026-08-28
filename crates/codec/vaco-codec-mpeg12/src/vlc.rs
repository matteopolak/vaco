//! One generic prefix-code decoder, shared by every table in [`crate::tables`].
//!
//! # Why linear scan, not a lookup table
//!
//! A production decoder builds a fast lookup (a small trie, or a table
//! indexed by the next N bits) once per table. This crate reads bit by bit
//! and rescans the active table at every bit instead — `O(bits × rows)` per
//! symbol, the obvious-but-simple starting point before reaching for a
//! faster shape. It has not been benchmarked against a trie because
//! correctness, not throughput, has been this package's priority.

use vaco_bitstream::BitReader;

use crate::tables::bits_of;

/// Read one prefix code from `r` against `table`, where each row supplies
/// `(bits, value)` via `key`. Returns `None` on a bitstream that never
/// matches any row within `max_len` bits — always a caller error (a
/// mis-selected table) or a corrupt/adversarial stream, never a valid code
/// that this function failed to find, since every table this crate uses is
/// checked prefix-free in `tables::tests`.
pub(crate) fn decode<'a, T>(
    r: &mut BitReader<'_>,
    table: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&'a T) -> (&'static str, u8),
    max_len: u8,
) -> Option<&'a T>
where
    T: 'a,
{
    // Pre-compute (code, len) once per row rather than per bit, and keep
    // the source row alongside so the caller gets its own type back.
    let rows: Vec<(u32, u8, &'a T)> = table
        .into_iter()
        .map(|row| {
            let (bits_str, _skip) = key(row);
            let (code, len) = bits_of(bits_str);
            (code, len, row)
        })
        .collect();

    let mut accum: u32 = 0;
    let mut len: u8 = 0;
    while len < max_len {
        accum = (accum << 1) | r.get_bit();
        len += 1;
        for &(code, code_len, row) in &rows {
            if code_len == len && code == accum {
                return Some(row);
            }
        }
        if r.check().is_err() {
            // The sticky-overrun reader ran off the end of the buffer;
            // further reads only manufacture zero bits forever, which
            // could otherwise spin until `max_len`. Bail immediately.
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_row_of_a_small_table() {
        let table: &[(&str, i32)] = &[("1", 1), ("01", 2), ("00", 3)];
        for &(bits, val) in table {
            let (code, len) = bits_of(bits);
            let bytes = (code << (32 - len)).to_be_bytes();
            let mut r = BitReader::new(&bytes);
            let got = decode(&mut r, table, |row| (row.0, 0), 8);
            assert_eq!(got.map(|r| r.1), Some(val));
        }
    }

    #[test]
    fn returns_none_on_exhausted_input() {
        let table: &[(&str, i32)] = &[("111", 1)];
        let bytes = [0u8; 4];
        let mut r = BitReader::new(&bytes);
        // All-zero input never matches a table whose only code is "111".
        assert!(decode(&mut r, table, |row| (row.0, 0), 8).is_none());
    }
}
