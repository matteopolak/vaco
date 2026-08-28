//! DCT token Huffman tables (`Vaco-Spec-Ref: theora-spec-20170603 section
//! 6.4.4`): each of the 80 tables is stored in the setup header as a binary
//! tree, decoded by reading one bit at a time — `0` for "this is an internal
//! node, descend further", `1` for "this is a leaf, the next 5 bits are the
//! token value". Building the tree once at setup time and walking it per
//! token is simpler and just as fast as re-deriving canonical codes, and it
//! is exactly the shape the spec itself describes.
//!
//! The spec caps a single code at 32 bits and a single table at 32 entries
//! (section 6.4.4) specifically to bound a recursive decoder's stack depth
//! against a hostile bitstream; both caps are enforced here during
//! construction, past which the reader is flagged malformed rather than
//! recursing further.

use vaco_bitstream::BitReader;

/// One node of a Huffman binary tree: a leaf token, or a split into `0`/`1`
/// subtrees.
#[derive(Debug, Clone)]
enum Node {
    Leaf(u8),
    Split(Box<Node>, Box<Node>),
}

/// One of the 80 DCT token Huffman tables.
#[derive(Debug, Clone)]
pub(crate) struct HuffTable {
    root: Node,
}

/// A code longer than this is undecodable by construction (section 6.4.4);
/// used both to cap recursion depth and as the entry-count guard's twin.
const MAX_CODE_LEN: u32 = 32;
/// No table may have more than this many entries (section 6.4.4).
const MAX_ENTRIES: u32 = 32;

impl HuffTable {
    /// Parse one table from the setup header bitstream (section 6.4.4).
    ///
    /// On a malformed tree (an oversized code or too many entries) this
    /// flags `r` malformed and returns *some* table rather than propagating
    /// an error immediately, matching [`vaco_bitstream::BitReader`]'s
    /// sticky-overrun convention: the caller checks once, after all 80
    /// tables are parsed, rather than threading a `Result` through a
    /// recursive tree builder.
    fn parse(r: &mut BitReader<'_>) -> Self {
        let mut entries = 0u32;
        let root = Self::parse_node(r, 0, &mut entries);
        Self { root }
    }

    fn parse_node(r: &mut BitReader<'_>, depth: u32, entries: &mut u32) -> Node {
        if depth > MAX_CODE_LEN {
            r.flag_malformed();
            return Node::Leaf(0);
        }
        if r.get_bit() != 0 {
            if *entries >= MAX_ENTRIES {
                r.flag_malformed();
                return Node::Leaf(0);
            }
            *entries += 1;
            let token = u8::try_from(r.get(5)).unwrap_or(0);
            Node::Leaf(token)
        } else {
            let left = Self::parse_node(r, depth + 1, entries);
            let right = Self::parse_node(r, depth + 1, entries);
            Node::Split(Box::new(left), Box::new(right))
        }
    }

    /// Walk the tree, one bit per level, until a leaf is reached.
    ///
    /// Terminates in at most [`MAX_CODE_LEN`] reads: [`Self::parse`] never
    /// builds a tree deeper than that. A read past the packet's end returns
    /// sticky zeros (always descending the `0` branch) rather than looping,
    /// so a truncated packet still terminates — the caller's `r.check()`
    /// after the frame is decoded is what catches it.
    pub(crate) fn decode(&self, r: &mut BitReader<'_>) -> u8 {
        let mut node = &self.root;
        loop {
            match node {
                Node::Leaf(t) => return *t,
                Node::Split(zero, one) => {
                    node = if r.get_bit() != 0 { one } else { zero };
                }
            }
        }
    }
}

/// Parse all 80 tables (section 6.4.4, step 1).
pub(crate) fn parse_tables(r: &mut BitReader<'_>) -> Box<[HuffTable; 80]> {
    let tables: Vec<HuffTable> = (0..80).map(|_| HuffTable::parse(r)).collect();
    // `Vec<HuffTable>` of length exactly 80 by construction above.
    if let Ok(arr) = tables.try_into() {
        arr
    } else {
        r.flag_malformed();
        // Unreachable in practice (the map above always yields 80 elements),
        // but a fallback avoids ever panicking on this path.
        Box::new(core::array::from_fn(|_| HuffTable {
            root: Node::Leaf(0),
        }))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    /// Build a tiny bitstream for a 2-entry table: token 5 at code `0`,
    /// token 9 at code `1`.
    #[test]
    fn two_entry_table_round_trips() {
        // split(0=1,leaf0)... build stream: ISLEAF=0 (split), then left:
        // ISLEAF=1, TOKEN=5 (00101); then right: ISLEAF=1, TOKEN=9 (01001).
        let mut bits = String::new();
        bits.push('0'); // split
        bits.push('1'); // left leaf
        bits.push_str("00101"); // token 5
        bits.push('1'); // right leaf
        bits.push_str("01001"); // token 9
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let table = HuffTable::parse(&mut r);

        let mut r2 = BitReader::new(&[0b0000_0000]);
        assert_eq!(table.decode(&mut r2), 5);
        let mut r3 = BitReader::new(&[0b1000_0000]);
        assert_eq!(table.decode(&mut r3), 9);
    }

    #[test]
    fn single_entry_table_is_the_empty_code() {
        // ISLEAF=1, TOKEN=3: the whole table is one leaf with a 0-bit code.
        let mut bits = String::from("1");
        bits.push_str("00011");
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let table = HuffTable::parse(&mut r);
        let mut r2 = BitReader::new(&[0xFF]);
        assert_eq!(table.decode(&mut r2), 3);
    }

    #[test]
    fn oversized_table_flags_malformed_without_panicking() {
        // A right-leaning "comb" tree of 40 leaves: `n` leaves is
        // `("0" + "1" + 5-bit-token)` repeated `n - 1` times (a split, whose
        // left branch is a leaf) followed by one final bare leaf. 40 exceeds
        // `MAX_ENTRIES` (32), so the parser must flag malformed partway
        // through rather than accept every leaf or panic.
        let mut bits = String::new();
        for _ in 0..39 {
            bits.push('0');
            bits.push('1');
            bits.push_str("00000");
        }
        bits.push('1');
        bits.push_str("00000");
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let _ = HuffTable::parse(&mut r);
        assert!(r.overrun());
    }

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = 0u8;
        let mut n = 0u32;
        for c in bits.chars() {
            cur = (cur << 1) | u8::from(c == '1');
            n += 1;
            if n == 8 {
                out.push(cur);
                cur = 0;
                n = 0;
            }
        }
        if n > 0 {
            cur <<= 8 - n;
            out.push(cur);
        }
        out
    }
}
