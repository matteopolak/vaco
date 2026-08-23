//! Property tests for round-trip invariants that hold across arbitrary
//! input, not just the worked examples in each module's own unit tests.

use proptest::prelude::*;
use vaco_format_audio_simple::extended80;
use vaco_format_audio_simple::pcm::frames_in;

proptest! {
    /// Any positive sample rate a real file would carry survives the 80-bit
    /// extended-precision round trip to within a relative error tiny enough
    /// that rounding to the nearest integer Hz recovers it exactly.
    #[test]
    fn extended80_round_trips_plausible_sample_rates(rate in 1.0f64..10_000_000.0) {
        let bytes = extended80::from_f64(rate);
        let back = extended80::to_f64(&bytes);
        prop_assert!((back - rate).abs() / rate < 1e-9);
    }

    /// `frames_in` never divides by the caller's zero (it is documented to
    /// treat `bytes_per_frame == 0` as `1`), and for a byte count that is an
    /// exact multiple of a non-zero frame width, it recovers the frame count
    /// exactly.
    #[test]
    fn frames_in_recovers_an_exact_frame_count(
        frame_count in 0u64..100_000,
        bytes_per_frame in 1u32..64,
    ) {
        let bytes = frame_count * u64::from(bytes_per_frame);
        prop_assert_eq!(frames_in(bytes, bytes_per_frame), frame_count);
    }

    /// A zero `bytes_per_frame` never panics (divide-by-zero), regardless of
    /// the byte count.
    #[test]
    fn frames_in_with_zero_frame_width_does_not_panic(bytes in 0u64..1_000_000) {
        let _ = frames_in(bytes, 0);
    }
}
