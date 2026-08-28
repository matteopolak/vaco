//! Huffman table construction and symbol decode (ITU-T T.81 Annex C, F.2.2.3).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.

use vaco_core::{Error, Result};

use crate::bits::EntropyReader;
use crate::tables::HuffSpec;

/// A `DHT`-defined (or Annex K default) Huffman table, ready to decode
/// symbols one bit at a time.
///
/// Built by Annex F.2.2.3's `generate_decoding_tables`: for every code length
/// `l` in `1..=16`, `min_code[l]`/`max_code[l]` bound the codes of that
/// length and `val_ptr[l]` is where their symbols start in `values`.
/// `max_code[l] == -1` marks a length with no codes at all.
#[derive(Debug, Clone)]
pub(crate) struct DecodeTable {
    min_code: [i32; 17],
    max_code: [i32; 17],
    val_ptr: [usize; 17],
    values: [u8; 256],
    value_count: usize,
}

impl DecodeTable {
    /// Build from a `BITS`/`HUFFVAL` pair.
    ///
    /// `values` is capped at 256 entries — the most a `DHT` field (one byte
    /// per count, 16 counts) can ever declare — so this never needs a
    /// budgeted allocation for a table this small.
    #[must_use]
    pub(crate) fn build(counts: &[u8; 16], values: &[u8]) -> Self {
        let mut huffsize = [0u8; 257];
        let mut n = 0usize;
        for (len, &count) in counts.iter().enumerate() {
            for _ in 0..count {
                if let Some(slot) = huffsize.get_mut(n) {
                    *slot = (len + 1) as u8;
                }
                n += 1;
            }
        }
        let total = n.min(256);

        let mut huffcode = [0i32; 257];
        let mut code = 0i32;
        let mut si = huffsize.first().copied().unwrap_or(0);
        let mut k = 0usize;
        while k < total {
            while huffsize.get(k).copied() == Some(si) {
                if let Some(slot) = huffcode.get_mut(k) {
                    *slot = code;
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
            si += 1;
        }

        let mut min_code = [-1i32; 17];
        let mut max_code = [-1i32; 17];
        let mut val_ptr = [0usize; 17];
        let mut p = 0usize;
        for l in 1..=16usize {
            let cnt = usize::from(counts.get(l - 1).copied().unwrap_or(0));
            if cnt == 0 {
                continue;
            }
            if let Some(slot) = val_ptr.get_mut(l) {
                *slot = p;
            }
            if let Some(slot) = min_code.get_mut(l) {
                *slot = huffcode.get(p).copied().unwrap_or(0);
            }
            p += cnt - 1;
            if let Some(slot) = max_code.get_mut(l) {
                *slot = huffcode.get(p).copied().unwrap_or(0);
            }
            p += 1;
        }

        let mut table_values = [0u8; 256];
        let value_count = values.len().min(256);
        if let (Some(dst), Some(src)) = (
            table_values.get_mut(..value_count),
            values.get(..value_count),
        ) {
            dst.copy_from_slice(src);
        }

        Self {
            min_code,
            max_code,
            val_ptr,
            values: table_values,
            value_count,
        }
    }

    /// Build one of the Annex K default tables.
    #[must_use]
    pub(crate) fn from_spec(spec: &HuffSpec) -> Self {
        Self::build(&spec.counts, spec.values)
    }

    /// Decode one symbol, reading one bit at a time (F.2.2.3's `DECODE`).
    ///
    /// # Errors
    /// [`Error::InvalidData`] when 16 bits are read without matching any
    /// defined code, or when a matched code's symbol index was never
    /// populated — both mean the entropy-coded data or the table that is
    /// supposed to decode it are inconsistent.
    pub(crate) fn decode(&self, r: &mut EntropyReader<'_>) -> Result<u8> {
        let mut code = 0i32;
        for len in 1..=16usize {
            code = (code << 1) | r.get_bit().cast_signed();
            let max = self.max_code.get(len).copied().unwrap_or(-1);
            if max >= 0 && code <= max {
                let min = self.min_code.get(len).copied().unwrap_or(0);
                let base = self.val_ptr.get(len).copied().unwrap_or(0);
                let offset = usize::try_from(code - min).unwrap_or(usize::MAX);
                let idx = base.saturating_add(offset);
                if idx < self.value_count {
                    return self.values.get(idx).copied().ok_or(Error::InvalidData(
                        "jpeg: huffman symbol index out of range",
                    ));
                }
                return Err(Error::InvalidData(
                    "jpeg: huffman symbol index out of range",
                ));
            }
        }
        Err(Error::InvalidData(
            "jpeg: no huffman code matched in 16 bits",
        ))
    }
}

/// The encoder's mirror of [`DecodeTable`]: `(code length, code)` per symbol
/// value, built the same way (Annex C.2's `generate_size_table` +
/// `generate_code_table`) but indexed for the write direction instead.
#[derive(Debug, Clone)]
pub(crate) struct EncodeTable {
    codes: [Option<(u8, u16)>; 256],
}

impl EncodeTable {
    #[must_use]
    pub(crate) fn build(counts: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [None; 256];
        let mut code = 0u32;
        let mut k = 0usize;
        for (len_idx, &count) in counts.iter().enumerate() {
            let len = (len_idx + 1) as u8;
            for _ in 0..count {
                if let Some(&symbol) = values.get(k)
                    && let Some(slot) = codes.get_mut(usize::from(symbol))
                {
                    *slot = Some((len, code as u16));
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Self { codes }
    }

    #[must_use]
    pub(crate) fn from_spec(spec: &HuffSpec) -> Self {
        Self::build(&spec.counts, spec.values)
    }

    /// `(length, code)` for `symbol`, or `None` if this table never assigns
    /// it a code — which means the frame that built the table never chose to
    /// emit it.
    #[must_use]
    pub(crate) fn code_for(&self, symbol: u8) -> Option<(u8, u16)> {
        self.codes.get(usize::from(symbol)).copied().flatten()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use crate::tables::STD_DC_LUMA;

    #[test]
    fn decode_and_encode_tables_agree_on_every_symbol() {
        let dec = DecodeTable::from_spec(&STD_DC_LUMA);
        let enc = EncodeTable::from_spec(&STD_DC_LUMA);

        for &symbol in STD_DC_LUMA.values {
            let (len, code) = enc.code_for(symbol).expect("std table symbol has a code");
            let mut w = crate::bits::EntropyWriter::new();
            w.put_bits(u32::from(len), u32::from(code));
            w.flush_to_byte();
            let bytes = w.finish();
            let mut r = EntropyReader::new(&bytes, 0);
            assert_eq!(dec.decode(&mut r).unwrap(), symbol);
        }
    }

    #[test]
    fn garbage_bits_never_panic_the_decoder() {
        let dec = DecodeTable::from_spec(&STD_DC_LUMA);
        for pattern in [0x00u8, 0xFFu8, 0xAAu8, 0x55u8] {
            let bytes = [pattern; 4];
            let mut r = EntropyReader::new(&bytes, 0);
            let _ = dec.decode(&mut r);
        }
    }
}
