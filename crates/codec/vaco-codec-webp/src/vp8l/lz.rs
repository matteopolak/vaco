//! A minimal single-candidate LZ77 matcher over the ARGB pixel array.
//!
//! VP8L's backward references (spec §5.2.2) count in *pixels*, not bytes:
//! a match is a run of identical `u32` ARGB values, and this matcher works
//! directly on that slice.
//!
//! This is deliberately the simplest matcher that still finds real repeats
//! (flat regions, gradients, repeated rows): one candidate position per hash
//! bucket, no chaining. A hash chain would find longer/more matches, but
//! costs an allocation proportional to the pixel count — for a lossless
//! codec whose bar is "correct and interoperable", not "as dense as
//! `cwebp`", the fixed-size table below is the better trade: memory use is
//! `O(1)` in image size, and every candidate it does propose is verified
//! pixel-by-pixel before use, so a hash collision can only cost a missed
//! match, never a wrong one.
//!
//! One further simplification: only the *first* pixel of an accepted match
//! is hashed before we jump past it, so a later match cannot start inside an
//! earlier one. That gives up some compression for a matcher with no
//! internal state beyond one fixed-size table.

use vaco_core::Result;
use vaco_limits::Budget;

const MIN_MATCH: usize = 3;
/// Spec's own cap on a single backward reference (24 length prefix codes
/// cover exactly this range).
const MAX_MATCH: usize = 4096;
/// The largest *pixel* distance this matcher will propose. Kept at
/// `1_048_576 - 120` rather than the raw prefix-code ceiling, because this
/// crate's own encoder always writes a distance code as `distance + 120`
/// (it never uses the 2D neighbourhood codes 1..=120) — see
/// `codes::value_to_prefix`'s caller in `mod.rs`.
const MAX_DISTANCE: usize = 1_048_576 - 120;
const HASH_BITS: u32 = 16;

fn hash3(a: u32, b: u32, c: u32) -> usize {
    let h = a
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(b.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(c.wrapping_mul(0xC2B2_AE35));
    (h >> (32 - HASH_BITS)) as usize
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Match {
    pub(crate) length: usize,
    pub(crate) distance: usize,
}

#[derive(Debug)]
pub(crate) struct Matcher {
    head: Vec<i64>,
}

impl Matcher {
    pub(crate) fn new(budget: &mut Budget) -> Result<Self> {
        let mut head: Vec<i64> = budget.alloc(1usize << HASH_BITS)?;
        head.fill(-1);
        Ok(Self { head })
    }

    /// Look for a match starting at `pos`, then record `pos` under its own
    /// hash for future calls (whether or not it matched).
    pub(crate) fn find_and_insert(&mut self, pixels: &[u32], pos: usize) -> Option<Match> {
        if pos.saturating_add(MIN_MATCH) > pixels.len() {
            return None;
        }
        let (p0, p1, p2) = (
            *pixels.get(pos)?,
            *pixels.get(pos + 1)?,
            *pixels.get(pos + 2)?,
        );
        let h = hash3(p0, p1, p2);
        let candidate = self.head.get(h).copied().unwrap_or(-1);
        if let Some(slot) = self.head.get_mut(h) {
            *slot = i64::try_from(pos).unwrap_or(-1);
        }
        if candidate < 0 {
            return None;
        }
        let c = usize::try_from(candidate).unwrap_or(pos);
        if c >= pos || pos - c > MAX_DISTANCE {
            return None;
        }
        let max_len = pixels.len().saturating_sub(pos).min(MAX_MATCH);
        let mut len = 0usize;
        while len < max_len && pixels.get(c + len) == pixels.get(pos + len) {
            len += 1;
        }
        if len >= MIN_MATCH {
            Some(Match {
                length: len,
                distance: pos - c,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn finds_a_repeated_run() {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut m = Matcher::new(&mut budget).unwrap();
        let pixels = vec![1u32, 2, 3, 4, 5, 1, 2, 3, 4, 5, 9];
        // The *first* match is the one worth pinning exactly: pos 5 repeats
        // the first five pixels verbatim. Scanning does not stop there —
        // pos 6/7 also match (shorter, against different earlier
        // candidates), which is expected single-candidate-matcher
        // behaviour, not a defect, so this test does not assert on them.
        let mut found = None;
        for pos in 0..pixels.len() {
            if let Some(mtch) = m.find_and_insert(&pixels, pos) {
                found = Some((pos, mtch));
                break;
            }
        }
        let (pos, mtch) = found.expect("a repeat of the first five pixels should be found");
        assert_eq!(pos, 5);
        assert_eq!(mtch.distance, 5);
        assert_eq!(mtch.length, 5);
    }

    #[test]
    fn no_match_below_min_length_or_at_the_start() {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut m = Matcher::new(&mut budget).unwrap();
        let pixels = vec![7u32, 8, 7, 8, 9];
        for pos in 0..pixels.len() {
            // A 2-pixel alternation never reaches MIN_MATCH=3.
            assert!(m.find_and_insert(&pixels, pos).is_none());
        }
    }
}
