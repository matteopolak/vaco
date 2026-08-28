//! One generic prefix-code decoder, shared by every table in
//! [`crate::tables`]. Linear scan, not a lookup table — the same
//! obvious-but-simple starting point `vaco-codec-mpeg12`'s own `vlc`
//! module uses, for the same reason: correctness first.

use vaco_bitstream::BitReader;

use crate::tables::bits_of;

/// Read one prefix code from `r` against `table`, where each row supplies
/// `(bits, value)` via `key`. Returns `None` on a bitstream that never
/// matches any row within `max_len` bits.
pub(crate) fn decode<'a, T>(
    r: &mut BitReader<'_>,
    table: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&'a T) -> &'static str,
    max_len: u8,
) -> Option<&'a T>
where
    T: 'a,
{
    let rows: Vec<(u32, u8, &'a T)> = table
        .into_iter()
        .map(|row| {
            let (code, len) = bits_of(key(row));
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
            let got = decode(&mut r, table, |row| row.0, 8);
            assert_eq!(got.map(|r| r.1), Some(val));
        }
    }

    #[test]
    fn returns_none_on_exhausted_input() {
        let table: &[(&str, i32)] = &[("111", 1)];
        let bytes = [0u8; 4];
        let mut r = BitReader::new(&bytes);
        assert!(decode(&mut r, table, |row| row.0, 8).is_none());
    }
}
