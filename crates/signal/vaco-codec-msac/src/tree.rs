//! The tree-coded value, RFC 6386 §8.1 / VP9 spec §9.3.3.
//!
//! Both codecs represent a small alphabet as the leaves of a binary tree
//! stored as a flat array: each interior node occupies a pair of array
//! slots (its left and right branch), a positive entry is the index of a
//! deeper interior node, and a non-positive entry `v` is a leaf whose value
//! is `-v`. Walking the tree from the root, reading one bool per interior
//! node at the probability the caller supplies for that node, is the same
//! one-line algorithm in both specifications — this is the shared piece
//! `vaco-codec-msac` exists to hold once.

/// A tree specification: pairs of branch entries, root at index 0.
pub type Tree = [i8];

/// Read one bool at probability `probs[i]` and return `false`/`true`.
///
/// The caller supplies this as a closure so `read_tree` stays decoder-agnostic
/// — [`crate::vp8::BoolDecoder`] and [`crate::vp9::BoolDecoder`] both fit,
/// despite having no common trait (their bit-refill rules differ enough that
/// a shared trait would buy nothing — see the crate doc).
///
/// Walks `tree` from the root (index 0), calling `read_bool(node >> 1)` at
/// each interior node until a non-positive (leaf) entry is reached, and
/// returns its negation. A malformed tree (a positive entry that runs off
/// the end of the array) is treated as an immediate leaf of value 0 rather
/// than indexed out of bounds — untrusted probability tables and encoder
/// bugs alike must not panic here.
pub fn read_tree(tree: &Tree, read_bool: impl FnMut(usize) -> bool) -> i32 {
    read_tree_at(tree, 0, read_bool)
}

/// [`read_tree`], but starting the walk at tree index `start` instead of the
/// root. RFC 6386 §13.2's coefficient tree needs this: after a `DCT_0`
/// token, the next token's walk skips the EOB-vs-rest branch entirely
/// (`dct_eob` cannot follow a `DCT_0`), which is exactly "walk the same
/// tree starting two entries in" rather than a second tree.
pub fn read_tree_at(tree: &Tree, start: i32, mut read_bool: impl FnMut(usize) -> bool) -> i32 {
    let mut i: i32 = start;
    loop {
        let Ok(idx) = usize::try_from(i) else {
            return 0;
        };
        let node = idx >> 1;
        let branch = usize::from(read_bool(node));
        let Some(&entry) = tree.get(idx + branch) else {
            return 0;
        };
        if entry <= 0 {
            return -i32::from(entry);
        }
        i = i32::from(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6386 §8.2's `uv_mode_tree`: DC_PRED=0, V_PRED=1, H_PRED=2, TM_PRED=3.
    const UV_MODE_TREE: [i8; 6] = [-0, 2, -1, 4, -2, -3];

    #[test]
    fn walks_to_the_expected_leaf() {
        // "0" -> DC_PRED (0).
        assert_eq!(read_tree(&UV_MODE_TREE, |_| false), 0);
        // "10" -> V_PRED (1).
        let mut bits = [true, false].into_iter();
        assert_eq!(read_tree(&UV_MODE_TREE, |_| bits.next().unwrap_or(false)), 1);
        // "110" -> H_PRED (2).
        let mut bits = [true, true, false].into_iter();
        assert_eq!(read_tree(&UV_MODE_TREE, |_| bits.next().unwrap_or(false)), 2);
        // "111" -> TM_PRED (3).
        assert_eq!(read_tree(&UV_MODE_TREE, |_| true), 3);
    }

    #[test]
    fn a_malformed_tree_returns_a_leaf_instead_of_panicking() {
        let bad: [i8; 2] = [120, 120]; // positive entries pointing off the end
        assert_eq!(read_tree(&bad, |_| true), 0);
    }
}
