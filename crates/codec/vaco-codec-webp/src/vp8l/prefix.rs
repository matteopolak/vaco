//! Reading and writing one *transmitted* prefix code (spec §6.2.1): the
//! "simple" and "normal" code length codes, and the length-transmission RLE
//! (codes 16/17/18) the normal path can use.
//!
//! This crate's own encoder only ever writes the normal path with every
//! length spelled out literally (no 16/17/18 runs) — simpler and still
//! fully valid, since the RLE is an optional density optimisation, not a
//! structural requirement. The reader supports everything a compliant
//! bitstream (this crate's own, or `cwebp`'s) can send, since real files —
//! verified against `cwebp`/`dwebp` output — use the simple path and the RLE
//! runs freely.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use super::bitio::{BitReaderLsb, BitWriterLsb};
use super::huffman::{EncodeTable, HuffmanTable, lengths_from_freqs};

const CODE_LENGTH_CODES: usize = 19;
const CODE_LENGTH_ORDER: [usize; CODE_LENGTH_CODES] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// Read one prefix code (either transmission form) for an alphabet of
/// `alphabet_size` symbols.
///
/// # Errors
///
/// [`Error::InvalidData`] for a `max_symbol` or `color_cache_code_bits`
/// value the spec declares invalid, or a length exceeding 15.
pub(crate) fn read_prefix_code(
    r: &mut BitReaderLsb<'_>,
    alphabet_size: usize,
    budget: &mut Budget,
) -> Result<HuffmanTable> {
    let is_simple = r.read_bit() == 1;
    let mut lengths: Vec<u8> = budget.alloc(alphabet_size)?;
    if is_simple {
        let num_symbols = r.read_bits(1) + 1;
        let is_first_8bits = r.read_bits(1);
        let symbol0 = r.read_bits(1 + 7 * is_first_8bits) as usize;
        set_len1(&mut lengths, symbol0)?;
        if num_symbols == 2 {
            let symbol1 = r.read_bits(8) as usize;
            set_len1(&mut lengths, symbol1)?;
        }
        return HuffmanTable::from_lengths(&lengths);
    }

    let num_code_lengths = 4 + r.read_bits(4);
    let mut cl_lengths = vec![0u8; CODE_LENGTH_CODES];
    for i in 0..num_code_lengths as usize {
        let Some(&order_slot) = CODE_LENGTH_ORDER.get(i) else {
            break;
        };
        let l = r.read_bits(3) as u8;
        if let Some(slot) = cl_lengths.get_mut(order_slot) {
            *slot = l;
        }
    }
    let cl_table = HuffmanTable::from_lengths(&cl_lengths)?;

    let max_symbol = if r.read_bit() == 0 {
        alphabet_size as u32
    } else {
        let length_nbits = 2 + 2 * r.read_bits(3);
        2 + r.read_bits(length_nbits)
    };
    if max_symbol as usize > alphabet_size {
        return Err(Error::InvalidData("vp8l: max_symbol exceeds alphabet size"));
    }

    let mut symbol_idx: usize = 0;
    let mut prev_nonzero: u8 = 8;
    while symbol_idx < alphabet_size && symbol_idx < max_symbol as usize {
        if r.overran() {
            return Err(Error::UnexpectedEof);
        }
        let code_len_symbol = cl_table.decode(r);
        match code_len_symbol {
            0..=15 => {
                let l = code_len_symbol as u8;
                if let Some(slot) = lengths.get_mut(symbol_idx) {
                    *slot = l;
                }
                if l > 0 {
                    prev_nonzero = l;
                }
                symbol_idx += 1;
            }
            16 => {
                let repeat = 3 + r.read_bits(2) as usize;
                for _ in 0..repeat {
                    if symbol_idx >= alphabet_size {
                        break;
                    }
                    if let Some(slot) = lengths.get_mut(symbol_idx) {
                        *slot = prev_nonzero;
                    }
                    symbol_idx += 1;
                }
            }
            17 => {
                let repeat = 3 + r.read_bits(3) as usize;
                symbol_idx = (symbol_idx + repeat).min(alphabet_size);
            }
            18 => {
                let repeat = 11 + r.read_bits(7) as usize;
                symbol_idx = (symbol_idx + repeat).min(alphabet_size);
            }
            _ => return Err(Error::InvalidData("vp8l: bad code-length symbol")),
        }
    }
    HuffmanTable::from_lengths(&lengths)
}

fn set_len1(lengths: &mut [u8], symbol: usize) -> Result<()> {
    let Some(slot) = lengths.get_mut(symbol) else {
        return Err(Error::InvalidData(
            "vp8l: simple code length symbol out of range",
        ));
    };
    *slot = 1;
    Ok(())
}

/// Build an [`EncodeTable`] from symbol frequencies and write it using the
/// normal code length code, every length spelled out literally.
///
/// # Errors
///
/// Propagates a [`vaco_core::Error`] only if the derived lengths somehow
/// fail canonical assignment (never happens for output of
/// [`lengths_from_freqs`], which always returns a valid full code).
pub(crate) fn write_prefix_code(
    w: &mut BitWriterLsb,
    freqs: &[u64],
    alphabet_size: usize,
) -> Result<EncodeTable> {
    let lengths = lengths_from_freqs(freqs, 15);
    if lengths.len() != alphabet_size {
        return Err(Error::InvalidData("vp8l: frequency table size mismatch"));
    }
    let table = EncodeTable::new(lengths.clone())?;

    w.write_bit(0); // not a simple code length code
    w.write_bits(15, 4); // num_code_lengths = 4 + 15 = 19: always write every slot

    // Huffman-code the 19 possible length VALUES (0..=15) by how often they
    // occur among `lengths`; this crate never emits repeat codes 16/17/18,
    // so their frequency is always zero and they cost the minimum the
    // canonical assignment allows.
    let mut cl_freqs = [0u64; CODE_LENGTH_CODES];
    for &l in &lengths {
        if let Some(slot) = cl_freqs.get_mut(l as usize) {
            *slot += 1;
        }
    }
    let cl_lengths = lengths_from_freqs(&cl_freqs, 7); // spec: code_length_code_lengths are 3-bit fields
    let cl_table = EncodeTable::new(cl_lengths.clone())?;
    for &ord in &CODE_LENGTH_ORDER {
        let l = cl_lengths.get(ord).copied().unwrap_or(0);
        w.write_bits(u32::from(l), 3);
    }

    w.write_bit(0); // no max_symbol optimisation: every symbol is transmitted
    for &l in &lengths {
        cl_table.write(w, l as usize);
    }
    Ok(table)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn write_then_read_round_trips_an_uneven_distribution() {
        let alphabet = 280usize;
        let mut freqs = vec![0u64; alphabet];
        freqs[0] = 1000;
        freqs[1] = 500;
        freqs[2] = 10;
        freqs[279] = 1;
        let mut w = BitWriterLsb::new();
        let table = write_prefix_code(&mut w, &freqs, alphabet).unwrap();
        // Encode a handful of symbols after the header so decode has
        // something to read back.
        for sym in [0usize, 1, 2, 0, 279] {
            table.write(&mut w, sym);
        }
        let bytes = w.finish();
        let mut r = BitReaderLsb::new(&bytes);
        let mut budget = Budget::new(Limits::permissive());
        let decoded = read_prefix_code(&mut r, alphabet, &mut budget).unwrap();
        for expect in [0u32, 1, 2, 0, 279] {
            assert_eq!(decoded.decode(&mut r), expect);
        }
        assert!(!r.overran());
    }

    #[test]
    fn single_symbol_alphabet_round_trips_with_no_payload_bits() {
        let alphabet = 5usize;
        let freqs = vec![0u64, 7, 0, 0, 0];
        let mut w = BitWriterLsb::new();
        let table = write_prefix_code(&mut w, &freqs, alphabet).unwrap();
        table.write(&mut w, 1);
        table.write(&mut w, 1);
        let bytes = w.finish();
        let mut r = BitReaderLsb::new(&bytes);
        let mut budget = Budget::new(Limits::permissive());
        let decoded = read_prefix_code(&mut r, alphabet, &mut budget).unwrap();
        assert_eq!(decoded.decode(&mut r), 1);
        assert_eq!(decoded.decode(&mut r), 1);
    }
}
