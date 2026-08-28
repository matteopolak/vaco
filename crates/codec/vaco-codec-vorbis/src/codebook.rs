//! Codebook parsing and Huffman/VQ decode (Vorbis I spec section 3).
//!
//! `Vaco-Spec-Ref: vorbis-i sections 3.2.1 and 9.2.2/9.2.3`

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::bitreader::{BitReaderLsb, ilog};

/// A codeword-length list entry: `None` for a sparse codebook's unused slot.
type Lengths = Vec<Option<u8>>;

#[derive(Debug, Clone, Copy, Default)]
enum TrieNode {
    Internal {
        left: u32,
        right: u32,
    },
    Leaf(u32),
    #[default]
    Empty,
}

/// One parsed, decode-ready codebook.
#[derive(Debug, Clone)]
pub(crate) struct Codebook {
    pub(crate) dimensions: u32,
    pub(crate) entries: u32,
    /// `nodes[0]` is the root. `Empty` marks a child that was never assigned
    /// (an invalid codeword no well-formed encoder would emit).
    nodes: Vec<TrieNode>,
    /// `Some(entry)` when this book has exactly one used entry — the spec's
    /// single-entry special case (errata 20150226): decode always consumes
    /// exactly one bit, of either value, and returns that entry.
    single_entry: Option<u32>,
    /// VQ lookup vectors, one per entry, present only for lookup types 1/2.
    lookup: Option<Vec<Vec<f32>>>,
}

impl Codebook {
    /// Parse one codebook from the setup header bitstream (spec section
    /// 3.2.1). Any end-of-packet condition during setup renders the whole
    /// stream undecodable, so this returns `Err` rather than a partial book.
    pub(crate) fn parse(r: &mut BitReaderLsb<'_>, budget: &mut Budget) -> Result<Self> {
        let sync = r.get(24);
        if sync != 0x0056_4342 {
            return Err(Error::InvalidData("vorbis: codebook sync pattern mismatch"));
        }
        let dimensions = r.get(16);
        let entries = r.get(24);
        budget.consume_fuel(u64::from(entries).saturating_add(64))?;

        let ordered = r.get_bool();
        let mut lengths: Lengths = budget.alloc(entries as usize)?;
        if ordered {
            let mut current_entry: u64 = 0;
            let mut current_length = r.get(5).saturating_add(1);
            while current_entry < u64::from(entries) {
                let remaining = i64::from(entries)
                    .saturating_sub(i64::try_from(current_entry).unwrap_or(i64::MAX));
                let bits = ilog(remaining);
                let number = u64::from(r.get(bits));
                if r.overran() {
                    return Err(Error::InvalidData("vorbis: eop decoding ordered codebook"));
                }
                let end = current_entry.saturating_add(number).min(u64::from(entries));
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "current_entry..end is bounded by entries, a u32"
                )]
                for slot in lengths
                    .get_mut(current_entry as usize..end as usize)
                    .unwrap_or(&mut [])
                {
                    *slot = Some(current_length.min(255) as u8);
                }
                current_entry = current_entry.saturating_add(number);
                if number == 0 && current_entry >= u64::from(entries) {
                    break;
                }
                current_length = current_length.saturating_add(1);
                if current_length > 32 {
                    return Err(Error::InvalidData("vorbis: codeword length too long"));
                }
            }
            if current_entry > u64::from(entries) {
                return Err(Error::InvalidData(
                    "vorbis: ordered codebook overran entries",
                ));
            }
        } else {
            let sparse = r.get_bool();
            for slot in &mut lengths {
                if sparse {
                    if r.get_bool() {
                        *slot = Some(r.get(5).saturating_add(1).min(32) as u8);
                    } else {
                        *slot = None;
                    }
                } else {
                    *slot = Some(r.get(5).saturating_add(1).min(32) as u8);
                }
            }
        }
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding codebook lengths"));
        }

        let (nodes, single_entry) = build_trie(&lengths, budget)?;

        let lookup_type = r.get(4);
        let lookup = match lookup_type {
            0 => None,
            1 | 2 => Some(parse_lookup(r, budget, lookup_type, entries, dimensions)?),
            _ => {
                return Err(Error::InvalidData(
                    "vorbis: codebook lookup type greater than 2",
                ));
            }
        };
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding codebook lookup"));
        }

        Ok(Self {
            dimensions,
            entries,
            nodes,
            single_entry,
            lookup,
        })
    }

    /// Whether this book has a VQ value mapping (lookup type 1 or 2).
    #[must_use]
    pub(crate) const fn has_lookup(&self) -> bool {
        self.lookup.is_some()
    }

    /// Decode one Huffman codeword into an entry number (scalar context,
    /// spec section 3.3). `None` on end-of-packet or an invalid codeword.
    pub(crate) fn decode_scalar(&self, r: &mut BitReaderLsb<'_>) -> Option<u32> {
        if let Some(entry) = self.single_entry {
            let _ = r.read_tree_bit();
            return Some(entry);
        }
        if self.nodes.is_empty() {
            return None;
        }
        let mut node = 0u32;
        loop {
            match self.nodes.get(node as usize)? {
                TrieNode::Leaf(entry) => return Some(*entry),
                TrieNode::Empty => return None,
                TrieNode::Internal { left, right } => {
                    if r.overran() {
                        return None;
                    }
                    let bit = r.read_tree_bit();
                    node = if bit == 0 { *left } else { *right };
                }
            }
        }
    }

    /// Decode one codeword and return its VQ vector (spec section 3.3).
    /// `None` on end-of-packet, an invalid codeword, or a lookup-type-0 book
    /// (a decode error per spec: "requesting decode using a codebook of
    /// lookup type 0 in any context expecting a vector return is forbidden").
    pub(crate) fn decode_vector(&self, r: &mut BitReaderLsb<'_>) -> Option<&[f32]> {
        let entry = self.decode_scalar(r)?;
        self.lookup
            .as_ref()
            .and_then(|v| v.get(entry as usize))
            .map(Vec::as_slice)
    }
}

/// Build the canonical Huffman decode trie from a codeword-length list (spec
/// section 3.2.1: "each used codebook entry is assigned, in order, the
/// lowest valued unused binary Huffman codeword possible").
///
/// Entries arrive in original array order, not grouped by length, so
/// "lowest available codeword of the required length" cannot be tracked
/// with a single running counter — a short codeword that appears after
/// several long ones must still find whatever slot is actually free, which
/// may be behind slots the long ones already claimed. [`place`] does the
/// real thing: a leftmost-first-fit search of the trie being built, with
/// backtracking. This is the standard fact about prefix codes that makes it
/// always succeed whenever the lengths satisfy the Kraft inequality,
/// independent of insertion order — checked separately, and exactly, below.
fn build_trie(lengths: &[Option<u8>], budget: &mut Budget) -> Result<(Vec<TrieNode>, Option<u32>)> {
    const WIDTH: u32 = 32;
    let used: Vec<(u32, u8)> = lengths
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.map(|len| (i as u32, len)))
        .collect();

    if used.len() == 1 {
        let &(entry, len) = used.first().ok_or(Error::InvalidData(
            "vorbis: unreachable empty single-entry list",
        ))?;
        if len != 1 {
            return Err(Error::InvalidData(
                "vorbis: single-entry codebook must have codeword length 1",
            ));
        }
        return Ok((Vec::new(), Some(entry)));
    }
    if used.is_empty() {
        return Ok((Vec::new(), None));
    }

    // Kraft-sum completeness check, in exact fixed-point (units of 2^-32):
    // a valid, neither-under-nor-over-specified length list sums to exactly
    // one full code space.
    let mut total: u64 = 0;
    for &(_, len) in &used {
        if len == 0 || u32::from(len) > WIDTH {
            return Err(Error::InvalidData("vorbis: codeword length out of range"));
        }
        let contribution = 1u64 << (WIDTH - u32::from(len));
        total = total
            .checked_add(contribution)
            .filter(|&t| t <= 1u64 << WIDTH)
            .ok_or(Error::InvalidData("vorbis: overspecified huffman tree"))?;
    }
    if total != 1u64 << WIDTH {
        return Err(Error::InvalidData("vorbis: underspecified huffman tree"));
    }

    // `budget.alloc` default-initialises to `TrieNode::Empty` (index 0, the root).
    let mut nodes: Vec<TrieNode> = budget.alloc(1)?;
    for &(entry, len) in &used {
        budget.consume_fuel(u64::from(len))?;
        if !place(&mut nodes, 0, len, entry, budget)? {
            return Err(Error::InvalidData("vorbis: overspecified huffman tree"));
        }
    }
    Ok((nodes, None))
}

/// Leftmost-first-fit: find the smallest-valued still-empty codeword of
/// exactly `depth` bits under `node`, creating internal nodes as the search
/// descends, and claim it for `entry`. `false` means no such slot exists
/// (this entry's length conflicts with earlier placements).
fn place(
    nodes: &mut Vec<TrieNode>,
    node: usize,
    depth: u8,
    entry: u32,
    budget: &mut Budget,
) -> Result<bool> {
    budget.consume_fuel(1)?;
    if depth == 0 {
        return Ok(match nodes.get(node) {
            Some(TrieNode::Empty) => {
                if let Some(slot) = nodes.get_mut(node) {
                    *slot = TrieNode::Leaf(entry);
                }
                true
            }
            _ => false,
        });
    }
    let (left, right) = match nodes.get(node).copied().unwrap_or_default() {
        TrieNode::Leaf(_) => return Ok(false),
        TrieNode::Internal { left, right } => (left, right),
        TrieNode::Empty => {
            let left = new_node(nodes, budget)?;
            let right = new_node(nodes, budget)?;
            if let Some(slot) = nodes.get_mut(node) {
                *slot = TrieNode::Internal { left, right };
            }
            (left, right)
        }
    };
    if place(nodes, left as usize, depth - 1, entry, budget)? {
        return Ok(true);
    }
    place(nodes, right as usize, depth - 1, entry, budget)
}

fn new_node(nodes: &mut Vec<TrieNode>, budget: &mut Budget) -> Result<u32> {
    let extra: Vec<TrieNode> = budget.alloc(1)?;
    nodes.extend(extra);
    u32::try_from(nodes.len().saturating_sub(1))
        .map_err(|_| Error::InvalidData("vorbis: codebook too large"))
}

/// `float32_unpack` (spec section 9.2.2).
fn float32_unpack(x: u32) -> f32 {
    let mantissa = f64::from(x & 0x001f_ffff);
    let sign = x & 0x8000_0000;
    let exponent = (x & 0x7fe0_0000) >> 21;
    let mantissa = if sign != 0 { -mantissa } else { mantissa };
    let exponent = i32::try_from(exponent)
        .unwrap_or(i32::MAX)
        .saturating_sub(788);
    (mantissa * 2f64.powi(exponent)) as f32
}

/// `lookup1_values` (spec section 9.2.3): the greatest `r` with `r^dims <=
/// entries`.
fn lookup1_values(entries: u32, dims: u32) -> u32 {
    if dims == 0 {
        return 0;
    }
    let mut r = (f64::from(entries).powf(1.0 / f64::from(dims))) as u32;
    // Float rounding can land one off either side; walk to the exact answer.
    while r > 0 && checked_pow(r, dims).is_none_or(|v| v > u64::from(entries)) {
        r -= 1;
    }
    while checked_pow(r + 1, dims).is_some_and(|v| v <= u64::from(entries)) {
        r += 1;
    }
    r
}

fn checked_pow(base: u32, exp: u32) -> Option<u64> {
    let mut result: u64 = 1;
    let base = u64::from(base);
    for _ in 0..exp {
        result = result.checked_mul(base)?;
        if result > u64::from(u32::MAX) * 2 {
            return Some(result);
        }
    }
    Some(result)
}

#[allow(
    clippy::integer_division,
    reason = "spec 3.2.1's lookup-type-1 vector unpack is defined as exact integer division/modulo"
)]
fn parse_lookup(
    r: &mut BitReaderLsb<'_>,
    budget: &mut Budget,
    lookup_type: u32,
    entries: u32,
    dimensions: u32,
) -> Result<Vec<Vec<f32>>> {
    let min_value = float32_unpack(r.get(32));
    let delta_value = float32_unpack(r.get(32));
    let value_bits = r.get(4).saturating_add(1);
    let sequence_p = r.get_bool();

    let lookup_values = if lookup_type == 1 {
        lookup1_values(entries, dimensions)
    } else {
        entries.saturating_mul(dimensions)
    };
    budget.consume_fuel(u64::from(lookup_values).saturating_add(64))?;
    let mut multiplicands: Vec<f32> = budget.alloc(lookup_values as usize)?;
    for slot in &mut multiplicands {
        *slot = r.get(value_bits) as f32;
    }
    if r.overran() {
        return Err(Error::InvalidData(
            "vorbis: eop decoding codebook multiplicands",
        ));
    }

    // `Vec<Vec<f32>>` cannot go through `Budget::alloc` (it needs `T: Copy`),
    // so the outer vector grows by ordinary `push`; each inner vector is
    // still sized through the budget, which is what actually bounds memory
    // here (`dimensions` and `entries` are both attacker-controlled).
    budget.charge(u64::from(entries).saturating_mul(24))?;
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    for lookup_offset in 0..entries as usize {
        let mut vector: Vec<f32> = budget.alloc(dimensions as usize)?;
        let mut last = 0f32;
        if lookup_type == 1 {
            let mut index_divisor: u64 = 1;
            for v in &mut vector {
                let multiplicand_offset = if lookup_values == 0 {
                    0
                } else {
                    ((lookup_offset as u64 / index_divisor) % u64::from(lookup_values)) as usize
                };
                let value = multiplicands
                    .get(multiplicand_offset)
                    .copied()
                    .unwrap_or(0.0)
                    * delta_value
                    + min_value
                    + last;
                *v = value;
                if sequence_p {
                    last = value;
                }
                index_divisor = index_divisor.saturating_mul(u64::from(lookup_values.max(1)));
            }
        } else {
            let base = lookup_offset.saturating_mul(dimensions as usize);
            for (i, v) in vector.iter_mut().enumerate() {
                let multiplicand_offset = base.saturating_add(i);
                let value = multiplicands
                    .get(multiplicand_offset)
                    .copied()
                    .unwrap_or(0.0)
                    * delta_value
                    + min_value
                    + last;
                *v = value;
                if sequence_p {
                    last = value;
                }
            }
        }
        vectors.push(vector);
    }
    Ok(vectors)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    /// Bit-pack a codeword-length list the "unordered, dense" way and check
    /// that decode reproduces the spec's own eight-entry worked example
    /// (section 3.2.1): entry `i` maps to the codeword table given there.
    fn build(lengths: &[u8]) -> (Vec<TrieNode>, Option<u32>) {
        let opts: Vec<Option<u8>> = lengths.iter().map(|&l| Some(l)).collect();
        let mut budget = Budget::new(Limits::permissive());
        build_trie(&opts, &mut budget).unwrap()
    }

    fn decode_from_bits(nodes: &[TrieNode], single: Option<u32>, bits: &[u32]) -> Option<u32> {
        if let Some(e) = single {
            return Some(e);
        }
        let mut node = 0usize;
        let mut iter = bits.iter();
        loop {
            match nodes.get(node)? {
                TrieNode::Leaf(e) => return Some(*e),
                TrieNode::Empty => return None,
                TrieNode::Internal { left, right } => {
                    let bit = *iter.next()?;
                    node = if bit == 0 {
                        *left as usize
                    } else {
                        *right as usize
                    };
                }
            }
        }
    }

    #[test]
    fn spec_example_codewords_decode_to_the_right_entries() {
        // lengths: 2 4 4 4 4 2 3 3 -> codewords 00 0100 0101 0110 0111 10 110 111
        let (nodes, single) = build(&[2, 4, 4, 4, 4, 2, 3, 3]);
        assert!(single.is_none());
        assert_eq!(decode_from_bits(&nodes, single, &[0, 0]), Some(0));
        assert_eq!(decode_from_bits(&nodes, single, &[0, 1, 0, 0]), Some(1));
        assert_eq!(decode_from_bits(&nodes, single, &[0, 1, 0, 1]), Some(2));
        assert_eq!(decode_from_bits(&nodes, single, &[0, 1, 1, 0]), Some(3));
        assert_eq!(decode_from_bits(&nodes, single, &[0, 1, 1, 1]), Some(4));
        assert_eq!(decode_from_bits(&nodes, single, &[1, 0]), Some(5));
        assert_eq!(decode_from_bits(&nodes, single, &[1, 1, 0]), Some(6));
        assert_eq!(decode_from_bits(&nodes, single, &[1, 1, 1]), Some(7));
    }

    #[test]
    fn underspecified_tree_is_rejected() {
        let opts: Vec<Option<u8>> = vec![Some(2), Some(4), Some(4), Some(4)]; // missing entry 4/7 pairing -> incomplete
        let mut budget = Budget::new(Limits::permissive());
        assert!(build_trie(&opts, &mut budget).is_err());
    }

    #[test]
    fn single_entry_codebook_requires_length_one() {
        let opts: Vec<Option<u8>> = vec![Some(3)];
        let mut budget = Budget::new(Limits::permissive());
        assert!(build_trie(&opts, &mut budget).is_err());
        let opts_ok: Vec<Option<u8>> = vec![Some(1)];
        let mut budget = Budget::new(Limits::permissive());
        let (nodes, single) = build_trie(&opts_ok, &mut budget).unwrap();
        assert!(nodes.is_empty());
        assert_eq!(single, Some(0));
    }

    #[test]
    fn lookup1_values_matches_definition() {
        assert_eq!(lookup1_values(256, 2), 16);
        assert_eq!(lookup1_values(243, 5), 3);
        assert_eq!(lookup1_values(1, 4), 1);
    }

    #[test]
    fn float32_unpack_zero_and_one() {
        assert_eq!(float32_unpack(0), 0.0);
        // exponent field 788 (bias) with mantissa 1 => 1.0
        let bits = 788u32 << 21 | 1;
        assert_eq!(float32_unpack(bits), 1.0);
    }
}
