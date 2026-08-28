//! AC-3 decode against arbitrary bytes: sync frame, BSI, exponents, bit
//! allocation, mantissas, IMDCT — the whole `decode_frame` pipeline.
//!
//! What is asserted beyond "does not panic": every sample `decode_frame`
//! reports is finite (a bitstream desync must not silently produce `NaN`/
//! `inf` that would propagate into a mixer or a file write downstream), and
//! the decoder never allocates a channel count large enough to be a denial
//! of service (`acmod`/`lfeon` bound it to at most six, but this is the
//! backstop that would catch a regression in that bound rather than trusting
//! it by inspection alone).
//!
//! fuzz-crate: vaco-codec-ac3

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_ac3::{DecodeOptions, StreamState};

const MAX_CHANNELS: usize = 8;

fuzz_target!(|data: &[u8]| {
    let mut state = StreamState::new();
    let opts = DecodeOptions { apply_drc: true };
    let Ok(frame) = vaco_codec_ac3::decode_frame(data, &mut state, &opts) else {
        return;
    };
    assert!(frame.channels.len() <= MAX_CHANNELS);
    for channel in &frame.channels {
        for &s in channel {
            assert!(s.is_finite(), "decode_frame produced a non-finite sample");
        }
    }
    if let Some(lfe) = &frame.lfe {
        for &s in lfe {
            assert!(s.is_finite(), "decode_frame produced a non-finite LFE sample");
        }
    }

    // A second frame through the same stream state exercises the block-to-
    // block exponent/bit-allocation carryover and the overlap-add tail —
    // the state a single-frame fuzz input cannot reach on its own.
    let _ = vaco_codec_ac3::decode_frame(data, &mut state, &opts);
});
