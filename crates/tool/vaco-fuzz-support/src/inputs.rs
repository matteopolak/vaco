//! Structured `arbitrary` input types shared across fuzz targets (plan 13
//! §2.3).
//!
//! # Why these exist
//!
//! Raw bytes are the right input for something that genuinely consumes a
//! byte stream — a demuxer, a protocol response, an elementary-stream
//! parser. For everything else, raw bytes waste almost the whole fuzzing
//! budget on inputs rejected in the first microsecond, and every target
//! that wants better input has, until now, hand-rolled its own `Arbitrary`
//! enum (see `fuzz/fuzz_targets/bitreader_arbitrary.rs` for one). This
//! module is the shared vocabulary plan 13 §2.3 asks for, so the next
//! target reaches for `Dim`/`BoundedBytes` instead of writing a fifth copy.
//!
//! # `Dim` — biased to edges
//!
//! Off-by-one bugs cluster at powers of two and at format-specific block
//! boundaries (16 for macroblocks, 64 for CTUs, 128 for AV1 superblocks). A
//! uniformly random `u32` almost never lands on one. `Dim` spends a third of
//! its budget on a curated edge-value table and the rest on a uniform draw
//! across the caller's declared range.
//!
//! # `BoundedBytes` — the AV1-loop-OOM class, structurally
//!
//! AGENT-CONSTRAINTS.md's `BitReader::get` story is the general failure
//! shape: an `arbitrary`-generated `Vec<u8>` can be as large as the fuzzer's
//! input, so a struct with several `Vec<u8>` fields can amplify a small
//! input into a huge allocation before any of this project's own
//! `vaco-limits` budgeting ever runs. `BoundedBytes<N>` caps the *generated*
//! length at `N`, so the amplification cannot happen in the input layer at
//! all — independent of whether the code under test also budgets correctly,
//! which is the property `limit_*` fuzz targets check separately.

use arbitrary::{Arbitrary, Unstructured};

/// A dimension-shaped integer, biased toward values that find off-by-one and
/// block-boundary bugs. Always in `0..=MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dim<const MAX: u32>(u32);

impl<const MAX: u32> Dim<MAX> {
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl<const MAX: u32> From<Dim<MAX>> for u32 {
    fn from(d: Dim<MAX>) -> Self {
        d.0
    }
}

/// The edge values worth over-representing, before clamping to `0..=MAX`.
/// Zero, the first few integers (off-by-one territory), and the neighbours
/// of the block sizes this project's codecs actually use (16 macroblocks,
/// 64 CTUs, 128 AV1 superblocks) plus classic power-of-two neighbours.
const EDGE_CANDIDATES: [u32; 15] = [
    0, 1, 2, 3, 15, 16, 17, 63, 64, 65, 127, 128, 129, 4095, 4096,
];

impl<'a, const MAX: u32> Arbitrary<'a> for Dim<MAX> {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let use_edge: u8 = u.arbitrary()?;
        if use_edge.is_multiple_of(3) {
            let idx: u8 = u.arbitrary()?;
            let candidate = EDGE_CANDIDATES
                .get(idx as usize % EDGE_CANDIDATES.len())
                .copied()
                .unwrap_or(0);
            Ok(Self(candidate.min(MAX)))
        } else {
            let v = u.int_in_range(0..=MAX)?;
            Ok(Self(v))
        }
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (2, Some(5))
    }
}

/// A `Vec<u8>` whose `arbitrary`-generated length never exceeds `MAX` bytes,
/// regardless of how much input the fuzzer's own buffer contains.
///
/// This is the structural fix for the class of bug AGENT-CONSTRAINTS.md
/// describes under `BitReader::get`: a field typed as a plain `Vec<u8>` in
/// an `Arbitrary`-derived struct can be sized from the *entire* remaining
/// input, so three or four such fields on one struct can multiply a small
/// fuzzer input into a allocation many times its size before the code under
/// test ever gets a chance to apply its own limits. `BoundedBytes` caps that
/// at the type level, once, for every fuzz target that uses it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundedBytes<const MAX: usize>(Vec<u8>);

impl<const MAX: usize> BoundedBytes<MAX> {
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl<const MAX: usize> AsRef<[u8]> for BoundedBytes<MAX> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<'a, const MAX: usize> Arbitrary<'a> for BoundedBytes<MAX> {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let len = u.int_in_range(0..=MAX)?;
        let mut bytes = Vec::new();
        for _ in 0..len {
            bytes.push(u.arbitrary::<u8>()?);
        }
        Ok(Self(bytes))
    }

    fn size_hint(_depth: usize) -> (usize, Option<usize>) {
        (1, None)
    }
}

/// A minimal, codec-agnostic packet shape for structured decoder fuzzing
/// (plan 13 §2.3's `FuzzPacket`), generic over the payload cap so a target
/// with a smaller sane packet size than another can say so.
#[derive(Debug, Clone, Arbitrary)]
pub struct FuzzPacket<const MAX_PAYLOAD: usize> {
    pub data: BoundedBytes<MAX_PAYLOAD>,
    pub keyframe: bool,
    /// Small deltas exercise ordinary timestamp handling; the full `i16`
    /// range reaches overflow/wraparound paths without needing `i64`.
    pub pts_delta: i16,
    /// `(tag, payload)` generic side-data — deliberately not tied to any one
    /// crate's `SideDataKind` enum, so this type stays usable before that
    /// enum's crate is even a dependency.
    pub side_data: Vec<(u8, BoundedBytes<256>)>,
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use arbitrary::{Arbitrary, Unstructured};

    use super::{BoundedBytes, Dim, FuzzPacket};

    fn feed<T: for<'a> Arbitrary<'a>>(bytes: &[u8]) -> T {
        let mut u = Unstructured::new(bytes);
        T::arbitrary(&mut u).unwrap()
    }

    #[test]
    fn dim_never_exceeds_its_bound() {
        // Sweep a range of raw byte patterns rather than relying on any one
        // fixed seed to happen to hit both the edge-table and uniform paths.
        for b in 0_u8..=255 {
            let raw = vec![b; 64];
            let d: Dim<100> = feed(&raw);
            assert!(d.get() <= 100, "raw byte {b}: got {}", d.get());
        }
    }

    #[test]
    fn dim_can_reach_its_declared_edge_values() {
        use std::collections::HashSet;
        let mut seen: HashSet<u32> = HashSet::new();
        for a in 0_u8..=255 {
            for b in 0_u8..=255 {
                let raw = [a, b, a, b, a, b, a, b];
                let d: Dim<200> = feed(&raw);
                seen.insert(d.get());
            }
        }
        assert!(seen.contains(&0));
        assert!(seen.contains(&1));
        assert!(seen.contains(&16) || seen.contains(&17), "seen: {seen:?}");
    }

    #[test]
    fn bounded_bytes_never_exceeds_max() {
        let raw = vec![0xFF_u8; 10_000];
        let b: BoundedBytes<32> = feed(&raw);
        assert!(b.as_slice().len() <= 32);
    }

    #[test]
    fn bounded_bytes_handles_short_input_gracefully() {
        let mut u = Unstructured::new(&[]);
        let b = BoundedBytes::<32>::arbitrary(&mut u);
        assert!(b.is_ok());
    }

    #[test]
    fn fuzz_packet_round_trips_through_arbitrary() {
        let raw = vec![7_u8; 512];
        let p: FuzzPacket<64> = feed(&raw);
        assert!(p.data.as_slice().len() <= 64);
        for (_, payload) in &p.side_data {
            assert!(payload.as_slice().len() <= 256);
        }
    }
}
