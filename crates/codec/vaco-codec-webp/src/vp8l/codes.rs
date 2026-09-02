//! The length/distance prefix-code arithmetic shared by decode and encode
//! (spec §5.2.2), and the mapping between a distance *code* and an actual
//! scan-line pixel offset.

use super::bitio::BitReaderLsb;
use super::distance_map::DISTANCE_MAP;

/// Recover a (length or distance) value from a prefix code plus the extra
/// bits that follow it in the stream. Spec's own pseudocode, transcribed
/// directly.
pub(crate) fn prefix_to_value(prefix_code: u32, r: &mut BitReaderLsb<'_>) -> u32 {
    if prefix_code < 4 {
        return prefix_code + 1;
    }
    let extra_bits = (prefix_code - 2) >> 1;
    let offset = (2 + (prefix_code & 1)) << extra_bits;
    offset + r.read_bits(extra_bits) + 1
}

/// The inverse: given a value `>= 1`, the prefix code to write plus the
/// extra bits (value, bit count) that follow it. A linear scan over at most
/// 40 candidate codes — this runs once per emitted token, not a hot loop.
pub(crate) fn value_to_prefix(value: u32) -> (u32, u32, u32) {
    if value <= 4 {
        return (value.saturating_sub(1), 0, 0);
    }
    for prefix_code in 4..40u32 {
        let extra_bits = (prefix_code - 2) >> 1;
        let offset = (2 + (prefix_code & 1)) << extra_bits;
        let range = 1u32 << extra_bits;
        if value > offset && value <= offset.saturating_add(range) {
            return (prefix_code, value - offset - 1, extra_bits);
        }
    }
    // Unreachable for any value this crate's encoder ever proposes (match
    // length capped at 4096, distance code capped so `distance + 120` never
    // exceeds prefix 39's range — see `lz.rs`'s `MAX_DISTANCE`). Kept as a
    // clamped fallback (pinned to prefix 39's own offset, 786432) rather
    // than a panic on the off chance a caller passes something larger.
    (39, value.saturating_sub(786_433).min((1u32 << 18) - 1), 18)
}

/// Convert a decoded distance *code* (the value [`prefix_to_value`] produced
/// from prefix code #5) to an actual backward pixel offset.
pub(crate) fn distance_code_to_dist(distance_code: u32, image_width: u32) -> i64 {
    if distance_code > 120 {
        return i64::from(distance_code - 120);
    }
    let (xi, yi) = DISTANCE_MAP
        .get((distance_code.saturating_sub(1)) as usize)
        .copied()
        .unwrap_or((0, 1));
    let dist = i64::from(xi) + i64::from(yi) * i64::from(image_width);
    dist.max(1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::super::bitio::BitWriterLsb;
    use super::*;

    #[test]
    fn value_to_prefix_and_back_agree_for_every_length_in_range() {
        for value in 1u32..=4096 {
            let (code, extra, nbits) = value_to_prefix(value);
            let mut w = BitWriterLsb::new();
            w.write_bits(extra, nbits);
            let bytes = w.finish();
            let mut r = BitReaderLsb::new(&bytes);
            assert_eq!(prefix_to_value(code, &mut r), value, "value {value}");
        }
    }

    #[test]
    fn distance_code_121_is_one_pixel_past_the_neighbourhood() {
        assert_eq!(distance_code_to_dist(121, 100), 1);
        assert_eq!(distance_code_to_dist(221, 100), 101);
    }

    #[test]
    fn distance_code_1_is_the_pixel_directly_above() {
        assert_eq!(distance_code_to_dist(1, 100), 100);
    }
}
