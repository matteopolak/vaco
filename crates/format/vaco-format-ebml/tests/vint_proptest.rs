//! VINT round-tripping across the full width range.
//!
//! RFC 8794's data-size VINT can encode any value up to `2^56 - 2` in eight
//! octets — the `- 2` matters twice: once for the reserved all-ones "unknown
//! size" marker at each width, and once more because [`vint_min`] must never
//! choose a width whose all-ones value collides with a value that is not
//! actually meant to say "unknown". This file is the property check that
//! edge survives, plus the plain round trip at every width.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use proptest::prelude::*;
use vaco_format_ebml::{Size, read_id, read_signed_vint, read_size, signed_vint, vint, vint_min};

proptest! {
    /// Any 56-bit value survives `vint_min` -> `read_size`.
    #[test]
    fn vint_min_round_trips_the_full_width_range(v in 0u64..(1u64 << 56) - 2) {
        let bytes = vint_min(v);
        prop_assert!((1..=8).contains(&bytes.len()));
        let (size, used) = read_size(&bytes, 8).unwrap();
        prop_assert_eq!(size, Size::Known(v));
        prop_assert_eq!(used, bytes.len());
    }

    /// A value written at a fixed width, for every legal width, decodes back
    /// to itself — the all-ones value at that width is excluded, since that
    /// is the reserved "unknown size" marker, not a length that means itself.
    #[test]
    fn vint_at_a_fixed_width_round_trips(len in 1u8..=8, v in 0u64..u64::MAX) {
        let bits = 7u32 * u32::from(len);
        let max = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
        let v = v % max; // exclude the all-ones marker at this width
        let bytes = vint(v, len);
        prop_assert_eq!(bytes.len(), usize::from(len));
        let (size, used) = read_size(&bytes, 8).unwrap();
        prop_assert_eq!(size, Size::Known(v));
        prop_assert_eq!(used, usize::from(len));
    }

    /// The all-ones VINT is the unknown-size marker at every width, and is
    /// never produced by `vint_min` for any value it is asked to encode
    /// (`vint_min` always steps up a width before it would collide).
    #[test]
    fn vint_min_never_emits_the_unknown_marker(v in 0u64..(1u64 << 56) - 2) {
        let bytes = vint_min(v);
        let (size, _) = read_size(&bytes, 8).unwrap();
        prop_assert_ne!(size, Size::Unknown);
    }

    /// A signed lace delta round-trips for the range Matroska actually uses
    /// (frame sizes fit comfortably in `i32`, so deltas do too).
    #[test]
    fn signed_vint_round_trips(v in -(1i64 << 31)..(1i64 << 31)) {
        let bytes = signed_vint(v);
        let (got, used) = read_signed_vint(&bytes).unwrap();
        prop_assert_eq!(got, v);
        prop_assert_eq!(used, bytes.len());
    }

    /// An element ID built at a chosen VINT width decodes back through
    /// `read_id` to the same value and width, and `id_bytes` re-encodes the
    /// decoded value to the identical bytes.
    ///
    /// IDs are built with [`vint`] rather than drawn from an arbitrary `u32`
    /// because `id_bytes` classifies a value's width from the numeric ranges
    /// legitimate marker-bearing IDs fall into (RFC 8794 section 5's Class
    /// A/B/C/D) — an arbitrary integer outside those ranges is not a value
    /// `id_bytes` is meant to re-encode, the same way an arbitrary byte string
    /// is not a value a UTF-8 decoder is meant to accept.
    #[test]
    fn element_id_round_trips(len in 1u8..=4, data in 0u64..(1u64 << 28)) {
        let bits = 7u32 * u32::from(len);
        let v = data % (1u64 << bits);
        let bytes = vint(v, len);
        let (id, used) = read_id(&bytes, 4).unwrap();
        prop_assert_eq!(used, usize::from(len));
        let mut buf = [0u8; 4];
        if let Some(dst) = buf.get_mut(4 - bytes.len()..) {
            dst.copy_from_slice(&bytes);
        }
        prop_assert_eq!(id, u32::from_be_bytes(buf));
        prop_assert_eq!(vaco_format_ebml::id_bytes(id), bytes);
    }
}
