//! Parsing robustness: garbage and truncated input must return a decode
//! error, never panic. `vaco-codec-vorbis` is `#![forbid(unsafe_code)]` and
//! this crate's own clippy configuration denies `unwrap`/`expect`/`panic`/
//! indexing, so this is a property test over that discipline rather than a
//! search for a specific crash.

#![allow(clippy::unwrap_used, reason = "test code")]

use proptest::prelude::*;
use vaco_codec_core::Decoder;
use vaco_codec_vorbis::VorbisDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arbitrary bytes offered as `extradata` must never panic, regardless
    /// of whether they happen to parse.
    #[test]
    fn set_extradata_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut dec = VorbisDecoder::new(Limits::strict());
        let _ = dec.set_extradata(&bytes);
    }

    /// Arbitrary bytes offered as an audio packet, after a real
    /// identification/setup pair, must never panic — the exact scenario a
    /// truncated or corrupted stream produces mid-decode.
    #[test]
    fn send_packet_never_panics_after_real_headers(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut dec = VorbisDecoder::new(Limits::strict());
        dec.set_extradata(&real_extradata()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        let _ = dec.send_packet(Some(&packet));
        let _ = dec.receive_frame();
    }
}

/// A real Vorbis header triple, produced once by `ffmpeg` and pinned as
/// bytes so this property test needs no subprocess. Captured with
/// `ffmpeg -f lavfi -i sine=frequency=440:duration=1 -ac 2 -c:a vorbis
/// -strict -2 -f ogg -`, then the three header packets' Xiph-laced form
/// read back out of the Ogg container's own `extradata`.
fn real_extradata() -> Vec<u8> {
    include_bytes!("fixtures/stereo_sine_extradata.bin").to_vec()
}
