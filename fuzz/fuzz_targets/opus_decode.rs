//! Opus decode against arbitrary bytes: range-coder framing, CELT and SILK
//! bitstream syntax, hybrid combination — the whole `OpusDecoder::send_packet`
//! / `receive_frame` pipeline (RFC 6716).
//!
//! What is asserted beyond "does not panic": every sample a decoded audio
//! frame reports is finite (a desynced range decoder or an unstable SILK LPC
//! reconstruction must not silently produce `NaN`/`inf` that would
//! propagate into a mixer or a file write downstream).
//!
//! The `OpusHead` extradata is fixed (mono, 48 kHz, no pre-skip) rather than
//! fuzzed — `vaco-parse-opus` owns validating that record, and fuzzing it
//! here would mostly exercise that crate's own fuzz target instead of this
//! one's decode path. The fuzzed bytes become the packet payload directly,
//! fed twice through the same decoder to exercise cross-packet state (SILK's
//! LPC/LTP history, CELT's overlap-add memory, stereo prediction) that a
//! single-packet input cannot reach on its own.
//!
//! fuzz-crate: vaco-codec-opus

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_opus::OpusDecoder;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A minimal, valid `OpusHead` (RFC 7845 §5.1): mono, 48 kHz, no pre-skip,
/// no output gain, no channel mapping table.
const OPUS_HEAD: &[u8] = &[
    b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', // magic
    1,    // version
    1,    // channel count
    0, 0, // pre-skip
    0x80, 0xbb, 0x00, 0x00, // input sample rate (48000, little-endian)
    0, 0, // output gain
    0, // channel mapping family
];

fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > 4096 {
        return;
    }

    let mut dec = OpusDecoder::new(Limits::default());
    if dec.set_extradata(OPUS_HEAD).is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::default());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };

    for _ in 0..2 {
        if dec.send_packet(Some(&packet)).is_err() {
            return;
        }
        loop {
            match dec.receive_frame() {
                Ok(frame) => {
                    let FrameData::Audio { samples, .. } = &frame.data else { continue };
                    let samples = *samples as usize;
                    for ch in 0..2 {
                        let Some(plane) = frame.plane(ch) else { continue };
                        let Some(row) = plane.row(0) else { continue };
                        for i in 0..samples {
                            let off = i * 4;
                            let Some(b) = row.get(off..off + 4) else { break };
                            let Ok(raw) = b.try_into() else { break };
                            let v = f32::from_le_bytes(raw);
                            assert!(v.is_finite(), "decoded a non-finite sample");
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }
});
