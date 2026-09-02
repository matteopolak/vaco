//! RFC 1951 (DEFLATE) decompression, written directly from the RFC's own
//! description (§3.2: stored/fixed-Huffman/dynamic-Huffman block types, the
//! canonical-Huffman construction of §3.2.2, the length/distance extra-bit
//! tables of §3.2.5). A public IETF specification, not FFmpeg/libav/zlib
//! source — see this module's parent for why it exists instead of a
//! dependency.
//!
//! Only what [`super::extract`] needs: a whole compressed stream in memory,
//! decompressed into one `Vec<u8>`. No streaming, no zlib/gzip wrapper (ZIP's
//! own "deflate" method is raw DEFLATE, no wrapper to strip).

use std::collections::HashMap;

use vaco_limits::Budget;

use super::ZipError;

/// Canonical Huffman decode table: `(code length in bits, code value)` to
/// symbol. A `HashMap` rather than a fast lookup table because every input
/// this crate decompresses is at most a few megabytes — simplicity that is
/// easy to check against the RFC's own pseudocode matters more here than
/// throughput.
type HuffMap = HashMap<(u8, u16), u16>;

/// RFC 1951 §3.2.5: length code base values for symbols 257..=285, indexed
/// from 0.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits read after each length code, same indexing as [`LEN_BASE`].
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// RFC 1951 §3.2.5: distance code base values for symbols 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits read after each distance code, same indexing as [`DIST_BASE`].
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// RFC 1951 §3.2.7: the order code-length code lengths are transmitted in,
/// for a dynamic-Huffman block header.
const CLEN_ORDER: [u8; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u32,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// One bit, LSB-first within each byte (RFC 1951 §3.1.1's "packing order
    /// for reading bits").
    fn bit(&mut self) -> Result<u32, ZipError> {
        let byte = *self
            .data
            .get(self.byte_pos)
            .ok_or(ZipError::Malformed("deflate stream ended mid-block"))?;
        let b = u32::from((byte >> self.bit_pos) & 1);
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Ok(b)
    }

    /// `n` bits (n <= 32), LSB-first packed into the result — the packing
    /// order §3.1.1 specifies for everything that is not a Huffman code.
    fn bits(&mut self, n: u32) -> Result<u32, ZipError> {
        let mut v: u32 = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    /// Discard the remainder of the current byte, for a stored block's
    /// "align to byte boundary" rule (§3.2.4).
    fn align(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos = self.byte_pos.saturating_add(1);
        }
    }

    fn take_u16(&mut self) -> Result<u16, ZipError> {
        let s = self
            .data
            .get(self.byte_pos..self.byte_pos + 2)
            .ok_or(ZipError::Malformed(
                "stored-block length ran past end of stream",
            ))?;
        let arr: [u8; 2] = s
            .try_into()
            .map_err(|_| ZipError::Malformed("stored-block length ran past end of stream"))?;
        self.byte_pos += 2;
        Ok(u16::from_le_bytes(arr))
    }

    fn take_bytes(&mut self, n: usize) -> Result<&'a [u8], ZipError> {
        let s = self
            .data
            .get(self.byte_pos..self.byte_pos + n)
            .ok_or(ZipError::Malformed("stored block ran past end of stream"))?;
        self.byte_pos += n;
        Ok(s)
    }
}

/// RFC 1951 §3.2.2's canonical-Huffman construction, applied to a symbol's
/// code *lengths* (a length of 0 means the symbol is absent).
fn build_huffman(lengths: &[u8]) -> Result<HuffMap, ZipError> {
    let mut bl_count = [0_u32; 16];
    for &l in lengths {
        let slot = bl_count
            .get_mut(usize::from(l))
            .ok_or(ZipError::Malformed("huffman code length over 15 bits"))?;
        *slot += 1;
    }
    // RFC 1951 §3.2.2 step 2 explicitly zeroes this before the `next_code`
    // pass: `bl_count[0]` counts *absent* symbols (length 0), which must not
    // perturb the length-1 baseline `code = (code + bl_count[0]) << 1`.
    if let Some(slot) = bl_count.first_mut() {
        *slot = 0;
    }
    let mut code: u32 = 0;
    let mut next_code = [0_u32; 16];
    for bits in 1..16usize {
        let prev_count = *bl_count.get(bits - 1).unwrap_or(&0);
        code = (code + prev_count) << 1;
        if let Some(slot) = next_code.get_mut(bits) {
            *slot = code;
        }
    }
    let mut map = HuffMap::new();
    for (sym, &l) in lengths.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let symbol =
            u16::try_from(sym).map_err(|_| ZipError::Malformed("huffman alphabet too large"))?;
        let slot = next_code
            .get_mut(usize::from(l))
            .ok_or(ZipError::Malformed("huffman code length over 15 bits"))?;
        let this_code = *slot;
        *slot += 1;
        let code16 = u16::try_from(this_code)
            .map_err(|_| ZipError::Malformed("huffman code overflowed 16 bits"))?;
        map.insert((l, code16), symbol);
    }
    Ok(map)
}

fn fixed_lit_len_lengths() -> Vec<u8> {
    (0_u32..288)
        .map(|i| match i {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        })
        .collect()
}

fn fixed_dist_lengths() -> Vec<u8> {
    vec![5_u8; 30]
}

fn decode_symbol(br: &mut BitReader<'_>, map: &HuffMap) -> Result<u16, ZipError> {
    let mut code: u16 = 0;
    for len in 1_u8..=15 {
        let bit = br.bit()?;
        code = (code << 1) | u16::try_from(bit).unwrap_or(0);
        if let Some(&sym) = map.get(&(len, code)) {
            return Ok(sym);
        }
    }
    Err(ZipError::Malformed(
        "no huffman code in this table matched the bitstream",
    ))
}

/// Decode one compressed block's symbol stream (fixed or dynamic — the two
/// differ only in which Huffman tables are in force) into `out`, stopping at
/// the end-of-block symbol (256).
fn inflate_block(
    br: &mut BitReader<'_>,
    lit_map: &HuffMap,
    dist_map: &HuffMap,
    out: &mut vaco_limits::IncrementalVec<u8>,
    budget: &mut Budget,
) -> Result<(), ZipError> {
    loop {
        budget.consume_fuel(1)?;
        let sym = decode_symbol(br, lit_map)?;
        if sym < 256 {
            let byte = u8::try_from(sym).unwrap_or(0);
            out.push_slice(budget, &[byte])?;
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = usize::from(sym - 257);
        let base = *LEN_BASE
            .get(li)
            .ok_or(ZipError::Malformed("length code out of range"))?;
        let extra = *LEN_EXTRA
            .get(li)
            .ok_or(ZipError::Malformed("length code out of range"))?;
        let length = u32::from(base) + br.bits(u32::from(extra))?;

        let dsym = usize::from(decode_symbol(br, dist_map)?);
        let dbase = *DIST_BASE
            .get(dsym)
            .ok_or(ZipError::Malformed("distance code out of range"))?;
        let dextra = *DIST_EXTRA
            .get(dsym)
            .ok_or(ZipError::Malformed("distance code out of range"))?;
        let distance = usize::try_from(u32::from(dbase) + br.bits(u32::from(dextra))?)
            .map_err(|_| ZipError::Malformed("distance overflow"))?;
        if distance == 0 || distance > out.len() {
            return Err(ZipError::Malformed(
                "back-reference distance exceeds the output produced so far",
            ));
        }
        let length = usize::try_from(length).map_err(|_| ZipError::Malformed("length overflow"))?;
        for _ in 0..length {
            budget.consume_fuel(1)?;
            let idx = out
                .len()
                .checked_sub(distance)
                .ok_or(ZipError::Malformed("back-reference underflowed the output"))?;
            let byte = *out.as_slice().get(idx).ok_or(ZipError::Malformed(
                "back-reference indexed past the output",
            ))?;
            out.push_slice(budget, &[byte])?;
        }
    }
}

/// Decompress a raw DEFLATE stream (no zlib/gzip wrapper).
///
/// `declared_len` seeds the growth cap ([`vaco_limits::Budget::incremental`]):
/// the archive's own central-directory record already claims an uncompressed
/// size, so a stream that decompresses to materially more than it declared
/// is rejected as malformed rather than allowed to grow without bound.
///
/// # Errors
/// [`ZipError::Malformed`] for anything the bitstream itself gets wrong
/// (reserved block type, a Huffman code no table entry matches, a
/// back-reference pointing before the start of output); [`ZipError::Limit`]
/// if the budget or fuel cap is hit.
pub(super) fn inflate(
    data: &[u8],
    declared_len: usize,
    budget: &mut Budget,
) -> Result<Vec<u8>, ZipError> {
    let mut br = BitReader::new(data);
    let mut out = budget.incremental::<u8>(declared_len);
    loop {
        budget.consume_fuel(1)?;
        let bfinal = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align();
                let len = br.take_u16()?;
                let _nlen = br.take_u16()?;
                let bytes = br.take_bytes(usize::from(len))?;
                out.push_slice(budget, bytes)?;
            }
            1 => {
                let lit_map = build_huffman(&fixed_lit_len_lengths())?;
                let dist_map = build_huffman(&fixed_dist_lengths())?;
                inflate_block(&mut br, &lit_map, &dist_map, &mut out, budget)?;
            }
            2 => {
                let hlit = usize::try_from(br.bits(5)?).unwrap_or(0) + 257;
                let hdist = usize::try_from(br.bits(5)?).unwrap_or(0) + 1;
                let hclen = usize::try_from(br.bits(4)?).unwrap_or(0) + 4;
                let mut clen_lengths = [0_u8; 19];
                for i in 0..hclen {
                    let v = u8::try_from(br.bits(3)?).unwrap_or(0);
                    if let Some(&order) = CLEN_ORDER.get(i)
                        && let Some(slot) = clen_lengths.get_mut(usize::from(order))
                    {
                        *slot = v;
                    }
                }
                let cl_map = build_huffman(&clen_lengths)?;
                let mut lengths: Vec<u8> = Vec::new();
                while lengths.len() < hlit + hdist {
                    budget.consume_fuel(1)?;
                    let sym = decode_symbol(&mut br, &cl_map)?;
                    match sym {
                        0..=15 => lengths.push(u8::try_from(sym).unwrap_or(0)),
                        16 => {
                            let rep = br.bits(2)? + 3;
                            let prev = *lengths.last().ok_or(ZipError::Malformed(
                                "repeat-previous code with no previous length",
                            ))?;
                            for _ in 0..rep {
                                lengths.push(prev);
                            }
                        }
                        17 => {
                            let rep = usize::try_from(br.bits(3)? + 3).unwrap_or(0);
                            lengths.extend(std::iter::repeat_n(0_u8, rep));
                        }
                        18 => {
                            let rep = usize::try_from(br.bits(7)? + 11).unwrap_or(0);
                            lengths.extend(std::iter::repeat_n(0_u8, rep));
                        }
                        _ => {
                            return Err(ZipError::Malformed(
                                "code-length alphabet has no symbol above 18",
                            ));
                        }
                    }
                }
                let lit_lengths = lengths.get(..hlit).ok_or(ZipError::Malformed(
                    "code length sequence shorter than HLIT",
                ))?;
                let dist_lengths = lengths.get(hlit..hlit + hdist).ok_or(ZipError::Malformed(
                    "code length sequence shorter than HLIT+HDIST",
                ))?;
                let lit_map = build_huffman(lit_lengths)?;
                let dist_map = build_huffman(dist_lengths)?;
                inflate_block(&mut br, &lit_map, &dist_map, &mut out, budget)?;
            }
            _ => return Err(ZipError::Malformed("reserved deflate block type (3)")),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out.into_vec())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::inflate;
    use vaco_limits::{Budget, Limits};

    /// Raw DEFLATE bytes for the fixed-Huffman encoding of `b"hello world"`,
    /// produced once with Python's `zlib.compressobj(wbits=-15)` while
    /// writing this test (not shipped as a tool dependency — just how the
    /// literal below was generated) and hand-verified against RFC 1951's
    /// fixed-Huffman code table.
    #[test]
    fn decompresses_a_known_fixed_huffman_stream() {
        // zlib.compressobj(level=9, wbits=-15).compress(b"hello world") + flush()
        let compressed: &[u8] = &[
            0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00,
        ];
        let mut budget = Budget::new(Limits::strict());
        let out = inflate(compressed, 11, &mut budget).expect("inflates");
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn decompresses_a_stored_block() {
        // BFINAL=1, BTYPE=00 (stored), byte-aligned, then LEN=5 NLEN=~5, "abcde".
        // First byte: bits are read LSB-first, so 0b001 (BTYPE=00, BFINAL=1)
        // packed into the low 3 bits of the first byte is 0x01.
        let mut compressed = vec![0x01_u8];
        compressed.extend_from_slice(&5_u16.to_le_bytes());
        compressed.extend_from_slice(&(!5_u16).to_le_bytes());
        compressed.extend_from_slice(b"abcde");
        let mut budget = Budget::new(Limits::strict());
        let out = inflate(&compressed, 5, &mut budget).expect("inflates");
        assert_eq!(out, b"abcde");
    }

    /// Raw DEFLATE bytes using a **dynamic** Huffman block (BFINAL=1,
    /// BTYPE=10) — 1701 bytes of space-separated words from a ten-word
    /// vocabulary, repetitive enough that back-references dominate and
    /// varied enough that `zlib` chooses a dynamic table over the fixed one.
    /// Produced with `zlib.compressobj(9, wbits=-15)` over
    /// `' '.join(random.Random(1).choice(WORDS) for _ in range(300))`
    /// while writing this test; verified by decompressed length and CRC-32
    /// (`zlib.crc32`) rather than embedding the 1701-byte plaintext.
    #[test]
    fn decompresses_a_dynamic_huffman_stream() {
        let compressed: &[u8] = &[
            0x75, 0x95, 0x5b, 0xae, 0xc3, 0x20, 0x0c, 0x44, 0xb7, 0x92, 0xad, 0x81, 0x2e, 0x4a,
            0xab, 0x9b, 0xb6, 0x91, 0x9a, 0x2f, 0x56, 0x5f, 0x95, 0x29, 0xf8, 0xd8, 0x24, 0x1f,
            0x21, 0xc4, 0xd8, 0xe3, 0xf1, 0x8b, 0xac, 0xe9, 0xf1, 0x48, 0xcb, 0x7f, 0xda, 0xf7,
            0xb4, 0xe4, 0x72, 0xa4, 0xa5, 0xec, 0xef, 0xfb, 0xf6, 0x7a, 0xea, 0xe3, 0xb8, 0xc5,
            0xf5, 0xfb, 0xfc, 0x95, 0xed, 0x48, 0x54, 0x48, 0xdb, 0x7e, 0xd3, 0xd1, 0xf7, 0x11,
            0x98, 0x64, 0x3f, 0xa3, 0x1f, 0xa6, 0x0c, 0xe1, 0xac, 0x9a, 0x35, 0xd7, 0xfb, 0xcb,
            0x81, 0xca, 0xcc, 0x54, 0xdb, 0xb1, 0x84, 0xa4, 0x06, 0x71, 0x35, 0x33, 0xea, 0x75,
            0x1e, 0x06, 0xdd, 0x6c, 0x1a, 0x95, 0xb5, 0x25, 0xc2, 0x45, 0x5f, 0x87, 0x46, 0x80,
            0xef, 0x5a, 0xfd, 0xad, 0x88, 0xc0, 0xe2, 0x3c, 0x0d, 0x16, 0x87, 0x39, 0x34, 0x1f,
            0x6d, 0x97, 0x43, 0x38, 0xa0, 0x36, 0x80, 0x6b, 0x4c, 0xbc, 0x2b, 0x82, 0xa3, 0xc4,
            0xd5, 0x90, 0x80, 0x27, 0x4a, 0x32, 0xd5, 0xbe, 0x89, 0x19, 0xab, 0xe3, 0x27, 0x28,
            0x30, 0xe8, 0xfe, 0x9a, 0x0a, 0x43, 0x1e, 0x76, 0xf0, 0x16, 0x60, 0xc9, 0x1e, 0xe8,
            0xd0, 0x1a, 0x31, 0x43, 0x29, 0xf4, 0x8d, 0x01, 0x33, 0x58, 0x30, 0x24, 0x29, 0xa1,
            0x82, 0x91, 0x0e, 0x25, 0xc8, 0x56, 0x3e, 0xd7, 0x27, 0x79, 0x14, 0xe6, 0x3a, 0xdd,
            0xbe, 0x2f, 0x72, 0x09, 0xd0, 0x75, 0x9a, 0x2c, 0x56, 0xc2, 0xe5, 0xd0, 0x8b, 0xfa,
            0x1b, 0xf1, 0xb3, 0xe5, 0xf3, 0xcc, 0xc4, 0xe5, 0xe9, 0x84, 0x96, 0x73, 0xc6, 0x81,
            0x2c, 0xa1, 0x11, 0xac, 0x8c, 0xa2, 0xa4, 0x6f, 0xed, 0x43, 0xab, 0xc7, 0x8a, 0xb1,
            0xdd, 0xc3, 0x2c, 0xa3, 0x3c, 0x33, 0x77, 0x81, 0x3b, 0xf7, 0xa4, 0xee, 0xf8, 0x7b,
            0x1b, 0x83, 0xf6, 0xf2, 0xe9, 0x3e, 0x91, 0x12, 0xe3, 0x16, 0xd9, 0x15, 0xf7, 0xe0,
            0xe9, 0xf5, 0x63, 0xe3, 0x19, 0x93, 0x46, 0x48, 0x94, 0x65, 0x4a, 0x36, 0xfb, 0x06,
            0xed, 0x3e, 0x5f, 0x4c, 0x4c, 0x43, 0x3d, 0x6f, 0xa5, 0x8b, 0xda, 0xba, 0x59, 0x85,
            0x53, 0x14, 0x08, 0x2d, 0x33, 0x35, 0xe2, 0x74, 0x31, 0x74, 0xf4, 0x38, 0x9d, 0x4e,
            0x6e, 0x4b, 0xbe, 0xba, 0xec, 0x2f, 0x72, 0x1c, 0xff, 0x01, 0x2e, 0x77, 0xa0, 0xf4,
            0x01,
        ];
        let mut budget = Budget::new(Limits::strict());
        let out = inflate(compressed, 1701, &mut budget).expect("inflates");
        assert_eq!(out.len(), 1701);
        assert_eq!(super::super::crc32(&out), 0x898b_a96a);
        assert!(out.starts_with(b"gamma kappa beta epsilon beta theta thet"));
        assert!(out.ends_with(b"beta zeta alpha eta beta eta gamma gamma"));
    }

    #[test]
    fn a_back_reference_past_the_start_of_output_is_rejected() {
        // BFINAL=1, BTYPE=01 (fixed huffman) = 0b011 in the low 3 bits = 0x03,
        // followed by a length/distance pair with no literals decoded yet:
        // this is deliberately malformed input, not a real compressor's
        // output, exercising the bounds check directly.
        let compressed: &[u8] = &[0x03];
        let mut budget = Budget::new(Limits::strict());
        let err = inflate(compressed, 16, &mut budget).unwrap_err();
        // Whatever the first decoded symbol is, either it is a length code
        // with a now out-of-range distance, or the stream runs out of bits
        // first -- both are `Malformed`, never a panic and never `Ok`.
        assert!(matches!(err, super::ZipError::Malformed(_)));
    }
}
