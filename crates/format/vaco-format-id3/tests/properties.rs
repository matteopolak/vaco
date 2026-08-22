//! Property tests for the invariants the unit tests can only sample.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_format_id3::encoding::{self, Encoding};
use vaco_format_id3::id3v1::Id3v1Tag;
use vaco_format_id3::synchsafe;
use vaco_format_id3::tag::Id3v2Tag;
use vaco_format_id3::unsync;
use vaco_limits::{Budget, Limits};

fn synchsafe_bytes(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

proptest! {
    /// `synchsafe::decode` never produces a value outside 28 bits, for any
    /// four input bytes.
    #[test]
    fn synchsafe_decode_is_always_28_bits(bytes in any::<[u8; 4]>()) {
        prop_assert!(synchsafe::decode(bytes) <= 0x0FFF_FFFF);
    }

    /// A value that started out `<= 0x0FFF_FFFF` round-trips through
    /// encode-as-synchsafe-bytes then decode.
    #[test]
    fn synchsafe_round_trips_28_bit_values(n in 0u32..=0x0FFF_FFFF) {
        prop_assert_eq!(synchsafe::decode(synchsafe_bytes(n)), n);
    }

    /// Removing unsynchronisation can only ever shrink or preserve length,
    /// never grow it, for arbitrary bytes.
    #[test]
    fn unsync_remove_never_grows(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut budget = Budget::new(Limits::permissive());
        let out = unsync::remove(&bytes, &mut budget).unwrap();
        prop_assert!(out.len() <= bytes.len());
    }

    /// The result of `unsync::remove` contains no `$FF $00` pair — every one
    /// that existed was stripped.
    #[test]
    fn unsync_remove_leaves_no_ff_00_pairs(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut budget = Budget::new(Limits::permissive());
        let out = unsync::remove(&bytes, &mut budget).unwrap();
        prop_assert!(!out.windows(2).any(|w| w == [0xFF, 0x00]));
    }

    /// Every ISO-8859-1 byte decodes to some `char` and never panics, for
    /// any byte value.
    #[test]
    fn latin1_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let s = encoding::decode(Encoding::Latin1, &bytes);
        prop_assert_eq!(s.chars().count(), bytes.len());
    }

    /// UTF-8 decoding never panics on arbitrary bytes, valid or not.
    #[test]
    fn utf8_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = encoding::decode(Encoding::Utf8, &bytes);
    }

    /// UTF-16 (both variants) decoding never panics on arbitrary bytes of
    /// any length, including odd lengths.
    #[test]
    fn utf16_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..129)) {
        let _ = encoding::decode(Encoding::Utf16Be, &bytes);
        let _ = encoding::decode(Encoding::Utf16Bom, &bytes);
    }

    /// `read_terminated` never panics and never returns more bytes in
    /// `rest` than were in the input.
    #[test]
    fn read_terminated_never_panics_and_shrinks(
        bytes in prop::collection::vec(any::<u8>(), 0..128),
        enc_byte in 0u8..4,
    ) {
        let encoding = Encoding::from_byte(enc_byte).unwrap();
        let (_, rest) = encoding::read_terminated(encoding, &bytes);
        prop_assert!(rest.len() <= bytes.len());
    }

    /// `Id3v1Tag::parse` never panics on arbitrary 128-byte input.
    #[test]
    fn id3v1_parse_never_panics_on_128_bytes(bytes in prop::collection::vec(any::<u8>(), 128..=128)) {
        let _ = Id3v1Tag::parse(&bytes);
    }

    /// `Id3v1Tag::parse` never panics on arbitrary-length input (most
    /// lengths are simply rejected).
    #[test]
    fn id3v1_parse_never_panics_on_any_length(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = Id3v1Tag::parse(&bytes);
    }

    /// `Id3v2Tag::parse` never panics on arbitrary bytes, under a budget
    /// tight enough that some allocations must be rejected.
    #[test]
    fn id3v2_parse_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let mut budget = Budget::new(Limits::strict().with_alloc_total(4096).with_fuel(4096));
        let _ = Id3v2Tag::parse(&bytes, &mut budget);
    }

    /// A well-formed minimal v2.3 tag with one TIT2 frame always round-trips
    /// its title text through `Id3v2Tag::parse`, for any printable-ASCII
    /// title.
    #[test]
    fn a_well_formed_tit2_frame_round_trips(title in "[ -~]{0,64}") {
        let mut content = vec![0x00];
        content.extend_from_slice(title.as_bytes());
        let mut frame = b"TIT2".to_vec();
        frame.extend_from_slice(&(content.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&content);

        let mut data = b"ID3".to_vec();
        data.push(3);
        data.push(0);
        data.push(0);
        data.extend_from_slice(&synchsafe_bytes(frame.len() as u32));
        data.extend_from_slice(&frame);

        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        if title.is_empty() {
            prop_assert!(t.entries.is_empty());
        } else {
            prop_assert_eq!(t.entries, vec![("title".to_string(), title)]);
        }
    }
}
