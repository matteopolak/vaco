//! Deterministic corpus mutation and test-case minimisation.
//!
//! # Scope, honestly stated
//!
//! Plan 13 §2.4.5 describes format-aware mutators — truncate at a box
//! boundary, duplicate a box, splice a box from another file of the same
//! format. This crate does not know what a box, an EBML element or a NAL
//! unit is; teaching it would mean depending on every container/codec crate
//! from a generic corpus tool, which is backwards. What it *can* do
//! honestly:
//!
//! - the generic operator tail the plan itself ranks lowest-value but still
//!   real (bitflip, byteflip, chunk truncate/duplicate/splice, interesting-
//!   value insertion) — [`mutate`];
//! - a genuinely format-agnostic structural primitive: mutate **between**
//!   caller-supplied boundary offsets, never across one — [`mutate_at_boundaries`].
//!   A demuxer-aware caller that already knows where its boxes/elements start
//!   gets boundary-respecting mutation without this crate needing to know
//!   what a box is.
//!
//! Format-specific operators (box duplication, fourcc substitution, PTS
//! wraparound) belong in the format crate that understands the structure,
//! built on top of these primitives — not here.
//!
//! # Minimisation
//!
//! [`minimise`] is the classic delta-debugging algorithm (Zeller & Hildebrandt,
//! *ddmin*): shrink by removing ever-smaller chunks while a caller-supplied
//! predicate still reports the input interesting, bounded by an iteration
//! budget so a pathological predicate cannot loop forever. This is exactly
//! what a fuzz crash minimiser needs and, like the mutators, needs no format
//! knowledge — the predicate embodies "still crashes" / "still triggers the
//! divergence" / whatever the caller is chasing.

/// A single mutation operator applied by [`mutate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operator {
    BitFlip,
    ByteFlip,
    /// Overwrite a run of bytes with a small "interesting" integer pattern
    /// (`0x00`, `0xFF`, `0x7F`, or a boundary value like `0x80`) — these are
    /// the values most likely to flip a signed/unsigned or off-by-one check.
    InterestingValue,
    /// Delete a contiguous chunk.
    Truncate,
    /// Duplicate a contiguous chunk in place (insert a copy right after it).
    Duplicate,
}

const OPERATORS: [Operator; 5] = [
    Operator::BitFlip,
    Operator::ByteFlip,
    Operator::InterestingValue,
    Operator::Truncate,
    Operator::Duplicate,
];

const INTERESTING_BYTES: [u8; 4] = [0x00, 0xFF, 0x7F, 0x80];

/// A small, deterministic, dependency-free PRNG (`SplitMix64`).
///
/// Not offered as a general random source — see `vaco-core`'s own
/// `random_rgb` for the precedent of using this exact construction for a
/// decorative/deterministic generator with no statistical claim attached.
/// Here the property that matters is reproducibility: the same seed always
/// produces the same mutation, which is what makes a mutated corpus entry
/// regress cleanly.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
        self.0 = self.0.wrapping_add(GAMMA);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`, or `0` if `bound == 0`.
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }

    /// Pick one element of `items` uniformly. `items` must be non-empty; the
    /// fallback for an empty slice is `items[0]`'s type default via `Copy`
    /// bound only — callers here always pass a fixed non-empty array, so
    /// this never actually falls back, but the type stays a safe `Option`
    /// walk rather than an indexing expression.
    fn pick<T: Copy>(&mut self, items: &[T], fallback: T) -> T {
        let idx = self.below(items.len());
        items.get(idx).copied().unwrap_or(fallback)
    }
}

/// Apply one pseudo-randomly chosen operator at one pseudo-randomly chosen
/// position, seeded deterministically. Returns the mutated copy; `data` is
/// never modified in place, so a caller can retry with a fresh seed cheaply.
///
/// A `data` shorter than 1 byte is returned unchanged — there is nothing to
/// mutate.
#[must_use]
pub fn mutate(data: &[u8], seed: u64) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng::new(seed);
    let op = rng.pick(&OPERATORS, Operator::BitFlip);
    apply(data, op, &mut rng)
}

fn apply(data: &[u8], op: Operator, rng: &mut Rng) -> Vec<u8> {
    let mut out = data.to_vec();
    match op {
        Operator::BitFlip => {
            let i = rng.below(out.len());
            let bit = rng.below(8);
            if let Some(byte) = out.get_mut(i) {
                *byte ^= 1 << bit;
            }
        }
        Operator::ByteFlip => {
            let i = rng.below(out.len());
            if let Some(byte) = out.get_mut(i) {
                *byte = !*byte;
            }
        }
        Operator::InterestingValue => {
            let i = rng.below(out.len());
            let run = 1 + rng.below(4.min(out.len() - i).max(1));
            let value = rng.pick(&INTERESTING_BYTES, 0x00);
            let end = (i + run).min(out.len());
            if let Some(slice) = out.get_mut(i..end) {
                slice.fill(value);
            }
        }
        Operator::Truncate => {
            if out.len() > 1 {
                let i = rng.below(out.len());
                let run = 1 + rng.below((out.len() - i).max(1));
                let end = (i + run).min(out.len());
                out.drain(i..end);
            }
        }
        Operator::Duplicate => {
            let i = rng.below(out.len());
            let run = 1 + rng.below((out.len() - i).max(1));
            let end = (i + run).min(out.len());
            if let Some(chunk) = out.get(i..end) {
                let copy = chunk.to_vec();
                out.splice(end..end, copy);
            }
        }
    }
    out
}

/// Mutate `data` without ever letting a chosen span cross a boundary in
/// `boundaries` (sorted offsets into `data`, each `<= data.len()`, marking
/// where the caller's own structure — box, EBML element, NAL unit — starts).
///
/// Every one of [`mutate`]'s operators still applies, just confined to a
/// pseudo-randomly chosen segment between two consecutive boundaries (or the
/// whole buffer if `boundaries` is empty, in which case this is exactly
/// [`mutate`]).
#[must_use]
pub fn mutate_at_boundaries(data: &[u8], boundaries: &[usize], seed: u64) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng::new(seed);

    let mut edges: Vec<usize> = boundaries
        .iter()
        .copied()
        .filter(|&b| b <= data.len())
        .collect();
    edges.push(0);
    edges.push(data.len());
    edges.sort_unstable();
    edges.dedup();

    if edges.len() < 2 {
        return mutate(data, seed);
    }

    let segment = rng.below(edges.len() - 1);
    let (Some(&start), Some(&end)) = (edges.get(segment), edges.get(segment + 1)) else {
        return mutate(data, seed);
    };
    let Some(span) = data.get(start..end) else {
        return mutate(data, seed);
    };
    if span.is_empty() {
        return data.to_vec();
    }

    let op = rng.pick(&OPERATORS, Operator::BitFlip);
    let mutated_span = apply(span, op, &mut rng);

    let mut out = Vec::new();
    if let Some(prefix) = data.get(..start) {
        out.extend_from_slice(prefix);
    }
    out.extend_from_slice(&mutated_span);
    if let Some(suffix) = data.get(end..) {
        out.extend_from_slice(suffix);
    }
    out
}

/// Shrink `data` to a smaller input for which `interesting` still returns
/// `true`, via delta-debugging (ddmin). `interesting(data)` must hold for the
/// initial `data` — if it does not, `data` is returned unchanged.
///
/// `max_iterations` bounds the number of calls to `interesting` regardless of
/// input size, so a pathological or non-monotonic predicate cannot loop
/// forever; on exhaustion the smallest interesting input found so far is
/// returned.
#[must_use]
pub fn minimise(
    data: &[u8],
    mut interesting: impl FnMut(&[u8]) -> bool,
    max_iterations: usize,
) -> Vec<u8> {
    if !interesting(data) {
        return data.to_vec();
    }

    let mut current = data.to_vec();
    let mut chunk_count: usize = 2;
    let mut iterations = 0usize;

    'outer: while current.len() >= 2 && iterations < max_iterations {
        let chunk_size = current.len().div_ceil(chunk_count);
        if chunk_size == 0 {
            break;
        }
        let mut start = 0usize;
        let mut reduced_this_pass = false;

        while start < current.len() {
            if iterations >= max_iterations {
                break 'outer;
            }
            let end = (start + chunk_size).min(current.len());
            let mut candidate = Vec::new();
            if let Some(prefix) = current.get(..start) {
                candidate.extend_from_slice(prefix);
            }
            if let Some(suffix) = current.get(end..) {
                candidate.extend_from_slice(suffix);
            }
            iterations += 1;

            if !candidate.is_empty() && interesting(&candidate) {
                current = candidate;
                chunk_count = chunk_count.saturating_sub(1).max(2);
                reduced_this_pass = true;
                break;
            }
            start += chunk_size;
        }

        if !reduced_this_pass {
            if chunk_count >= current.len() {
                break;
            }
            chunk_count = (chunk_count * 2).min(current.len().max(2));
        }
    }

    current
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "test code with fixed, hand-verified in-bounds indices"
)]
mod tests {
    use super::{Operator, minimise, mutate, mutate_at_boundaries};

    #[test]
    fn mutate_empty_is_a_no_op() {
        assert_eq!(mutate(&[], 42), Vec::<u8>::new());
    }

    #[test]
    fn mutate_is_deterministic_for_a_fixed_seed() {
        let data = b"the quick brown fox jumps over the lazy dog".to_vec();
        let a = mutate(&data, 12345);
        let b = mutate(&data, 12345);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_usually_produce_different_output() {
        let data = b"the quick brown fox jumps over the lazy dog".to_vec();
        let outputs: Vec<Vec<u8>> = (0..8).map(|s| mutate(&data, s)).collect();
        assert!(outputs.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn boundary_mutation_never_touches_the_other_side_of_a_boundary() {
        let left = vec![0xAA_u8; 16];
        let right = vec![0xBB_u8; 16];
        let mut data = left.clone();
        data.extend_from_slice(&right);
        let boundaries = [16usize];

        for seed in 0..64 {
            let out = mutate_at_boundaries(&data, &boundaries, seed);
            // Exactly one segment is mutated per call, so the untouched
            // segment must survive intact at whichever end it did not move
            // away from: if the left segment was the one mutated, its length
            // may have changed but `right` still appears verbatim as the
            // tail; if the right segment was mutated, `left` still appears
            // verbatim as the head.
            let left_untouched = out.starts_with(&left);
            let right_untouched = out.ends_with(&right);
            assert!(
                left_untouched || right_untouched,
                "seed {seed}: neither side was left untouched: {out:?}"
            );
        }
    }

    #[test]
    fn minimise_finds_the_single_essential_byte() {
        // "interesting" iff the buffer still contains a 0xFF anywhere.
        let mut data = vec![0_u8; 50];
        data[37] = 0xFF;
        let min = minimise(&data, |d| d.contains(&0xFF), 10_000);
        assert_eq!(min, vec![0xFF]);
    }

    #[test]
    fn minimise_returns_input_unchanged_if_not_initially_interesting() {
        let data = vec![1_u8, 2, 3];
        let min = minimise(&data, |_| false, 100);
        assert_eq!(min, data);
    }

    #[test]
    fn minimise_respects_the_iteration_budget() {
        let data = vec![0xFF_u8; 1000];
        let mut calls = 0usize;
        let min = minimise(
            &data,
            |d| {
                calls += 1;
                d.contains(&0xFF)
            },
            5,
        );
        assert!(
            calls <= 6,
            "budget of 5 plus the initial check, got {calls}"
        );
        assert!(!min.is_empty());
    }

    #[test]
    fn every_operator_is_exercised_across_many_seeds() {
        use std::collections::HashSet;
        let data = vec![0x10_u8; 64];
        let mut seen: HashSet<Operator> = HashSet::new();
        for seed in 0..200 {
            let mut rng = super::Rng::new(seed);
            seen.insert(rng.pick(&super::OPERATORS, Operator::BitFlip));
        }
        assert_eq!(seen.len(), super::OPERATORS.len());
        // silence unused-import concerns in case the assertions above change
        let _ = mutate(&data, 0);
    }
}
