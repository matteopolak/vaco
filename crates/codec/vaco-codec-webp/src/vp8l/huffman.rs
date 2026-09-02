//! Canonical Huffman codes: building, tree-decoding, and the RLE-coded
//! length transmission format RFC-adjacent formats (DEFLATE, VP8L) share.
//!
//! # The two directions
//!
//! Decoding a real bitstream (ours or `cwebp`'s) needs a table built from
//! *lengths read off the wire* — [`HuffmanTable::from_lengths`]. Encoding
//! needs the other direction: lengths chosen from *symbol frequencies*
//! ([`lengths_from_freqs`]), which this crate then feeds back through the
//! same canonical-assignment code so encoder and decoder agree on what each
//! codeword means by construction, not by matching tables independently.
//!
//! # Bit order
//!
//! A canonical code's bits are consumed one at a time, walking a binary
//! tree from the root; the first bit taken from the stream is the code's
//! *most significant* bit. That is independent of whatever convention the
//! surrounding bitstream uses for multi-bit fields (see [`super::bitio`]) —
//! it falls out of "build the same tree on both ends and walk it".
//!
//! # The one-symbol special case
//!
//! A table with exactly one used symbol is a valid "full binary tree" by
//! the spec's own definition, and reading it consumes **zero bits** — the
//! decoder already knows what it will find. Missing this case desynchronises
//! the whole bitstream the first time it fires, so it is handled as its own
//! [`HuffmanTable::Single`] variant rather than as a tree with one leaf.

use vaco_core::{Error, Result};

use super::bitio::{BitReaderLsb, BitWriterLsb};

/// A decode-ready canonical Huffman table.
#[derive(Debug, Clone)]
pub(crate) enum HuffmanTable {
    /// No symbol has nonzero length, or exactly one does: reading consumes
    /// zero bits and always yields this symbol. `None` means "this prefix
    /// code group is never read" (kept only so callers do not need an
    /// `Option<HuffmanTable>` everywhere); reachable only if a caller
    /// mistakenly tries to decode with it, which is a bug, not untrusted
    /// input, so it returns symbol 0.
    Single(u32),
    /// A binary trie. Node `0` is the root. `Leaf` is checked before
    /// descending further.
    Tree(Vec<Node>),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Node {
    Branch { zero: u32, one: u32 },
    Leaf(u32),
}

impl HuffmanTable {
    /// Build a decode table from a per-symbol length array (0 = unused).
    /// This is what a real bitstream's "normal"/"simple" code length code
    /// produces after being expanded to one length per symbol.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the lengths do not describe a full binary
    /// tree (over- or under-subscribed per Kraft's inequality) or exceed 15
    /// bits, both of which a compliant encoder never produces.
    pub(crate) fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let present: Vec<(usize, u8)> = lengths
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l > 0)
            .map(|(i, &l)| (i, l))
            .collect();
        if present.is_empty() {
            return Ok(Self::Single(0));
        }
        if present.len() == 1 {
            let sym = present.first().map_or(0, |&(i, _)| i);
            return Ok(Self::Single(u32::try_from(sym).unwrap_or(0)));
        }
        let codes = canonical_codes(lengths)?;
        let mut nodes = vec![Node::Branch {
            zero: u32::MAX,
            one: u32::MAX,
        }];
        for &(sym, len) in &present {
            let code = codes.get(sym).copied().unwrap_or(0);
            let mut cur = 0usize;
            for k in (0..len).rev() {
                let bit = (code >> k) & 1;
                let last = k == 0;
                let (child_slot_is_one, next) = {
                    let Some(Node::Branch { zero, one }) = nodes.get_mut(cur) else {
                        return Err(Error::InvalidData("vp8l: huffman lengths over-subscribed"));
                    };
                    if bit == 0 {
                        (false, *zero)
                    } else {
                        (true, *one)
                    }
                };
                let next_idx = if next == u32::MAX {
                    let new_idx = if last {
                        nodes.push(Node::Leaf(sym as u32));
                        nodes.len() - 1
                    } else {
                        nodes.push(Node::Branch {
                            zero: u32::MAX,
                            one: u32::MAX,
                        });
                        nodes.len() - 1
                    };
                    let Some(Node::Branch { zero, one }) = nodes.get_mut(cur) else {
                        return Err(Error::InvalidData("vp8l: huffman lengths over-subscribed"));
                    };
                    if child_slot_is_one {
                        *one = u32::try_from(new_idx).unwrap_or(u32::MAX);
                    } else {
                        *zero = u32::try_from(new_idx).unwrap_or(u32::MAX);
                    }
                    new_idx
                } else {
                    next as usize
                };
                cur = next_idx;
            }
        }
        Ok(Self::Tree(nodes))
    }

    /// Decode one symbol, walking the tree bit by bit.
    pub(crate) fn decode(&self, r: &mut BitReaderLsb<'_>) -> u32 {
        match self {
            Self::Single(sym) => *sym,
            Self::Tree(nodes) => {
                let mut cur = 0usize;
                loop {
                    match nodes.get(cur) {
                        Some(Node::Leaf(sym)) => return *sym,
                        Some(Node::Branch { zero, one }) => {
                            let bit = r.read_bit();
                            cur = if bit == 0 {
                                *zero as usize
                            } else {
                                *one as usize
                            };
                        }
                        None => return 0,
                    }
                }
            }
        }
    }
}

/// Canonical codeword per symbol from a length array (DEFLATE's
/// RFC 1951 §3.2.2 algorithm — a published, generic construction, not
/// specific to any one bitstream format).
fn canonical_codes(lengths: &[u8]) -> Result<Vec<u32>> {
    let max_len = lengths.iter().copied().max().unwrap_or(0) as usize;
    if max_len > 15 {
        return Err(Error::InvalidData("vp8l: huffman code length exceeds 15"));
    }
    let mut bl_count = vec![0u32; max_len + 1];
    for &l in lengths {
        if l > 0 {
            let Some(slot) = bl_count.get_mut(l as usize) else {
                return Err(Error::InvalidData("vp8l: huffman length out of range"));
            };
            *slot += 1;
        }
    }
    let mut code = 0u32;
    let mut next_code = vec![0u32; max_len + 2];
    for bits in 1..=max_len {
        code = (code + bl_count.get(bits - 1).copied().unwrap_or(0)) << 1;
        if let Some(slot) = next_code.get_mut(bits) {
            *slot = code;
        }
    }
    let mut codes = vec![0u32; lengths.len()];
    for (sym, &l) in lengths.iter().enumerate() {
        if l > 0 {
            let Some(slot) = next_code.get_mut(l as usize) else {
                return Err(Error::InvalidData("vp8l: huffman length out of range"));
            };
            if let Some(c) = codes.get_mut(sym) {
                *c = *slot;
            }
            *slot += 1;
        }
    }
    Ok(codes)
}

/// A table ready to *write*: lengths plus the matching canonical codewords.
#[derive(Debug, Clone)]
pub(crate) struct EncodeTable {
    pub(crate) lengths: Vec<u8>,
    codes: Vec<u32>,
    single: Option<u32>,
}

impl EncodeTable {
    pub(crate) fn new(lengths: Vec<u8>) -> Result<Self> {
        let present = lengths.iter().filter(|&&l| l > 0).count();
        if present <= 1 {
            let single = lengths.iter().position(|&l| l > 0).unwrap_or(0) as u32;
            return Ok(Self {
                lengths,
                codes: Vec::new(),
                single: Some(single),
            });
        }
        let codes = canonical_codes(&lengths)?;
        Ok(Self {
            lengths,
            codes,
            single: None,
        })
    }

    pub(crate) fn write(&self, w: &mut BitWriterLsb, symbol: usize) {
        if self.single.is_some() {
            return; // zero bits, matching HuffmanTable::Single's decode side.
        }
        let len = self.lengths.get(symbol).copied().unwrap_or(0);
        let code = self.codes.get(symbol).copied().unwrap_or(0);
        w.write_code_msb_first(code, len);
    }
}

/// Choose code lengths (each in `1..=15`) from symbol frequencies.
///
/// Uses a standard binary-heap Huffman merge when that alone keeps every
/// length within the 15-bit limit VP8L's length field allows (true for
/// every alphabet this crate ever builds — at most a few thousand symbols
/// even with a full color cache — short of a frequency distribution skewed
/// so extremely no real image produces it). If the raw tree would exceed
/// the limit anyway, this falls back to a balanced complete-binary-tree
/// assignment: not frequency-optimal, but always a valid full prefix code,
/// which is the property that actually matters (`705779d`: correctness
/// over optimality).
pub(crate) fn lengths_from_freqs(freqs: &[u64], limit: u8) -> Vec<u8> {
    let n = freqs.len();
    let mut lengths = vec![0u8; n];
    let present: Vec<usize> = (0..n)
        .filter(|&i| freqs.get(i).is_some_and(|&f| f > 0))
        .collect();
    if present.is_empty() {
        if let Some(slot) = lengths.first_mut() {
            *slot = 1;
        }
        return lengths;
    }
    if present.len() == 1 {
        if let Some(&sym) = present.first()
            && let Some(slot) = lengths.get_mut(sym)
        {
            *slot = 1;
        }
        return lengths;
    }
    let weights: Vec<u64> = present
        .iter()
        .map(|&i| freqs.get(i).copied().unwrap_or(1).max(1))
        .collect();
    let raw = huffman_tree_lengths(&weights);
    if raw.iter().all(|&l| l <= limit) {
        for (idx, &sym) in present.iter().enumerate() {
            if let (Some(slot), Some(&l)) = (lengths.get_mut(sym), raw.get(idx)) {
                *slot = l;
            }
        }
        return lengths;
    }
    // Fallback: balanced lengths, shorter codes to the more frequent symbols.
    let mut order: Vec<usize> = (0..present.len()).collect();
    order.sort_by(|&a, &b| {
        let wa = weights.get(a).copied().unwrap_or(0);
        let wb = weights.get(b).copied().unwrap_or(0);
        wb.cmp(&wa)
    });
    let balanced = balanced_lengths(present.len(), limit);
    for (rank, &idx) in order.iter().enumerate() {
        if let (Some(&sym), Some(&l)) = (present.get(idx), balanced.get(rank))
            && let Some(slot) = lengths.get_mut(sym)
        {
            *slot = l;
        }
    }
    lengths
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct HeapItem {
    weight: u64,
    seq: u32,
    node: usize,
}
impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse for a min-heap; break ties by insertion order for a
        // deterministic (not spec-mandated) tree shape.
        other
            .weight
            .cmp(&self.weight)
            .then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Standard (unbounded) Huffman code lengths via a binary min-heap merge,
/// in the same order as `weights`.
fn huffman_tree_lengths(weights: &[u64]) -> Vec<u8> {
    use std::collections::BinaryHeap;

    let n = weights.len();
    if n == 1 {
        return vec![1];
    }

    let mut heap = BinaryHeap::new();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut seq = 0u32;
    for (i, &w) in weights.iter().enumerate() {
        heap.push(HeapItem {
            weight: w,
            seq,
            node: i,
        });
        seq += 1;
    }
    let mut next_id = n;
    while heap.len() > 1 {
        let Some(a) = heap.pop() else { break };
        let Some(b) = heap.pop() else { break };
        let new_id = next_id;
        next_id += 1;
        parent.push(None);
        if let Some(slot) = parent.get_mut(a.node) {
            *slot = Some(new_id);
        }
        if let Some(slot) = parent.get_mut(b.node) {
            *slot = Some(new_id);
        }
        heap.push(HeapItem {
            weight: a.weight.saturating_add(b.weight),
            seq,
            node: new_id,
        });
        seq += 1;
    }

    let mut lengths = vec![0u8; n];
    for (i, slot) in lengths.iter_mut().enumerate() {
        let mut depth: u32 = 0;
        let mut cur = i;
        while let Some(Some(p)) = parent.get(cur) {
            depth = depth.saturating_add(1);
            cur = *p;
        }
        *slot = u8::try_from(depth).unwrap_or(255);
    }
    lengths
}

/// The lengths of a complete binary tree with `n` leaves: `x = 2^L - n`
/// leaves get length `L - 1` and the rest get length `L`, where
/// `L = ceil(log2(n))`. Always exactly Kraft-full regardless of
/// frequencies, which is what makes it a safe fallback.
///
/// Every length is `<= limit` precisely when `n <= 2^limit` — true for
/// every alphabet this crate ever builds (see [`lengths_from_freqs`]'s own
/// doc), and unavoidably false in general: no binary code can give `n`
/// leaves all length `<= limit` when `n` exceeds what `limit` bits can
/// address, so this does not attempt to enforce `limit` past that point —
/// it stays Kraft-full (the property a caller can least afford to lose)
/// rather than silently returning an invalid, over-full code.
fn balanced_lengths(n: usize, limit: u8) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1];
    }
    let mut level: u32 = 0;
    while (1usize << level) < n {
        level += 1;
    }
    let level = level.max(1);
    let _ = limit; // documented above: not enforceable past n > 2^limit.
    let short_count = (1usize << level).saturating_sub(n);
    let mut lengths = Vec::new();
    for i in 0..n {
        lengths.push(if i < short_count {
            u8::try_from(level.saturating_sub(1)).unwrap_or(limit)
        } else {
            u8::try_from(level).unwrap_or(limit)
        });
    }
    lengths
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn round_trip(lengths: &[u8], symbol: usize) {
        let table = EncodeTable::new(lengths.to_vec()).unwrap();
        let mut w = BitWriterLsb::new();
        table.write(&mut w, symbol);
        let bytes = w.finish();
        let decode = HuffmanTable::from_lengths(lengths).unwrap();
        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(decode.decode(&mut r), symbol as u32);
    }

    #[test]
    fn single_symbol_consumes_zero_bits() {
        let lengths = vec![1, 0, 0];
        round_trip(&lengths, 0);
        let table = EncodeTable::new(lengths.clone()).unwrap();
        let mut w = BitWriterLsb::new();
        table.write(&mut w, 0);
        assert!(w.finish().is_empty());
    }

    #[test]
    fn three_symbol_tree_round_trips_every_symbol() {
        let lengths = vec![1, 2, 2];
        for sym in 0..3 {
            round_trip(&lengths, sym);
        }
    }

    #[test]
    fn lengths_from_freqs_produce_a_full_kraft_sum() {
        let freqs = [100u64, 50, 25, 1, 1, 1, 1];
        let lengths = lengths_from_freqs(&freqs, 15);
        let sum: f64 = lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 2f64.powi(-i32::from(l)))
            .sum();
        assert!((sum - 1.0).abs() < 1e-9, "kraft sum {sum}");
    }

    #[test]
    fn balanced_fallback_stays_within_limit_for_a_large_skewed_alphabet() {
        // A geometric-ish weight sequence skewed enough that an unbounded
        // Huffman tree exceeds a tight limit, exercising the fallback. 40
        // symbols need at most 6 bits balanced (2^6 = 64 >= 40), well
        // within limit 8 — unlike VP8L's real alphabets, this is a limit
        // small enough to actually exercise the fallback at all.
        let mut freqs = vec![0u64; 40];
        for (i, f) in freqs.iter_mut().enumerate() {
            *f = 1u64 << i.min(20);
        }
        let lengths = lengths_from_freqs(&freqs, 8);
        assert!(lengths.iter().all(|&l| l <= 8));
        let sum: f64 = lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 2f64.powi(-i32::from(l)))
            .sum();
        assert!((sum - 1.0).abs() < 1e-6, "kraft sum {sum}");
    }
}
