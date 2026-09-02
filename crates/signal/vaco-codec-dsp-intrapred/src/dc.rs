//! DC intra prediction: the average of the available top and/or left
//! reference samples. Every format that has a DC mode at all defines it
//! this way — an average is not an authorial choice, it is the only thing
//! "predict the block's mean level from its neighbours" can mean — so this
//! one function is the whole of it.

/// The DC-mode prediction value for a block, given up to `size` top
/// reference samples and up to `size` left reference samples.
///
/// - Both available: `round(sum(top) + sum(left)) / (2 * size)`.
/// - Only one side available: that side's own average.
/// - Neither available (the block sits at the frame's top-left corner,
///   before any samples have been decoded): the bit depth's mid-grey value
///   `1 << (bit_depth - 1)` — every format's own fallback for "nothing to
///   predict from yet".
///
/// `top`/`left` longer than `size` use only their first `size` entries;
/// shorter than `size` are treated as fully unavailable for that side
/// (matching a caller that passes an empty slice to mean "not available"
/// rather than trying to signal partial availability here).
#[must_use]
pub fn dc_predict(top: &[u16], left: &[u16], size: usize, bit_depth: u32) -> u16 {
    let top = if top.len() >= size {
        top.get(..size)
    } else {
        None
    };
    let left = if left.len() >= size {
        left.get(..size)
    } else {
        None
    };

    match (top, left) {
        (Some(t), Some(l)) => {
            let sum: u32 = t.iter().chain(l).map(|&v| u32::from(v)).sum();
            let count = u32::try_from(2 * size).unwrap_or(1).max(1);
            round_div(sum, count)
        }
        (Some(t), None) => average(t),
        (None, Some(l)) => average(l),
        (None, None) => mid_grey(bit_depth),
    }
}

fn average(samples: &[u16]) -> u16 {
    let sum: u32 = samples.iter().map(|&v| u32::from(v)).sum();
    let count = u32::try_from(samples.len()).unwrap_or(1).max(1);
    round_div(sum, count)
}

// `count` is always `2 * size` or `samples.len()`, both `.max(1)`'d by the
// two call sites, so this is a deliberate rounded average, not a
// truncation bug.
#[allow(
    clippy::integer_division,
    reason = "count is max(1)'d by both call sites; this is a rounded average, not truncation"
)]
pub(crate) fn round_div(sum: u32, count: u32) -> u16 {
    let v = (sum + count / 2) / count;
    u16::try_from(v).unwrap_or(u16::MAX)
}

pub(crate) fn mid_grey(bit_depth: u32) -> u16 {
    let shift = bit_depth.saturating_sub(1).min(15);
    1u16 << shift
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test fixtures index small fixed arrays; an out-of-range index here is itself a test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_averages_all_samples() {
        // top = [10, 20], left = [30, 40]; sum=100, count=4, avg=25.
        assert_eq!(dc_predict(&[10, 20], &[30, 40], 2, 8), 25);
    }

    #[test]
    fn rounds_to_nearest() {
        // top=[1,2], left=[] unavailable (shorter than size) -> average of
        // top only = round(3/2) = round(1.5) = 2 (round-half-up via the
        // `+ count/2` bias).
        assert_eq!(dc_predict(&[1, 2], &[], 2, 8), 2);
    }

    #[test]
    fn only_top_available() {
        assert_eq!(dc_predict(&[4, 6, 8, 10], &[], 4, 8), 7);
    }

    #[test]
    fn only_left_available() {
        assert_eq!(dc_predict(&[], &[4, 6, 8, 10], 4, 8), 7);
    }

    #[test]
    fn neither_available_is_mid_grey() {
        assert_eq!(dc_predict(&[], &[], 4, 8), 128);
        assert_eq!(dc_predict(&[], &[], 4, 10), 512);
    }

    #[test]
    fn uniform_input_reproduces_the_constant() {
        for v in [0u16, 1, 128, 255] {
            let top = [v; 8];
            let left = [v; 8];
            assert_eq!(dc_predict(&top, &left, 8, 8), v);
        }
    }

    #[test]
    fn extra_length_is_ignored_past_size() {
        // A caller-provided buffer longer than `size` (e.g. reused across
        // calls) must not pull in samples past the block's own extent.
        let top = [1u16, 1, 1, 1, 1000, 1000];
        assert_eq!(dc_predict(&top, &[], 4, 8), 1);
    }

    proptest::proptest! {
        #[test]
        fn dc_predict_never_panics(
            top in proptest::collection::vec(proptest::num::u16::ANY, 0..64),
            left in proptest::collection::vec(proptest::num::u16::ANY, 0..64),
            size in 0usize..64,
            bit_depth in 0u32..32,
        ) {
            let _ = dc_predict(&top, &left, size, bit_depth);
        }

        #[test]
        fn dc_predict_is_between_the_min_and_max_of_its_inputs(
            top in proptest::collection::vec(1u16..=254, 4),
            left in proptest::collection::vec(1u16..=254, 4),
        ) {
            // An average can never fall outside the range of the values
            // averaged -- true regardless of which side(s) are available.
            let all: Vec<u16> = top.iter().chain(left.iter()).copied().collect();
            let lo = *all.iter().min().unwrap_or(&0);
            let hi = *all.iter().max().unwrap_or(&0);
            let v = dc_predict(&top, &left, 4, 8);
            proptest::prop_assert!(v >= lo && v <= hi);
        }
    }
}
