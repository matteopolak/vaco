//! Property tests complementing the `teletext_hamming_decode` and
//! `teletext_packet_parse` fuzz targets: the same "never panics, output
//! stays in range" properties, runnable under plain `cargo test` (fuzzing
//! needs a nightly toolchain and is not part of the standard gate).

use proptest::prelude::*;
use vaco_codec_subtitle_teletext::TeletextDecoder;
use vaco_codec_subtitle_teletext::hamming;

proptest! {
    #[test]
    fn decode8_never_panics_and_stays_in_range(byte: u8) {
        let (nibble, _correction) = hamming::decode8(byte);
        prop_assert!(nibble < 16);
    }

    #[test]
    fn decode24_never_panics_and_stays_in_range(bytes: [u8; 3]) {
        let (value, _correction) = hamming::decode24(bytes);
        prop_assert!(value < (1 << 18));
    }

    #[test]
    fn decoder_push_never_panics(data: Vec<u8>) {
        let mut decoder = TeletextDecoder::new();
        let events = decoder.push(&data);
        prop_assert!(events.len() <= 8);
        let _ = decoder.finish();
    }
}
