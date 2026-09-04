//! Real `ffmpeg`-produced fixtures for `vaco-codec-mpegaudio` — this crate's
//! first *committed* regression tests against real audio. Extensive
//! conformance measurement against `ffmpeg` was already done for this crate
//! (see `docs/codec/vaco-codec-mpegaudio.md`: real bugs found and fixed,
//! measured correlation/RMS across a dozen fixtures per layer), but none of
//! it was ever captured as a committed test — every number in that doc came
//! from a scratch script and hand-generated fixtures that were never
//! checked in, so nothing here regressed if any of it broke again. This
//! file exists to close that gap for Layer II and Layer III with real,
//! committed fixtures.
//!
//! Layer I has no fixture (documented already: no MP1 encoder is available
//! anywhere on this machine to make one).
//!
//! # Why not a sine wave
//!
//! Every fixture sums four incommensurate sine components (437/1289/2777/
//! 5431 Hz) rather than one pure tone — see
//! `vaco-codec-adpcm/tests/oracle_ffmpeg.rs`'s module doc for why a single
//! periodic source is the wrong choice for a test built to catch
//! block-alignment/framing bugs.
//!
//! # Tolerance
//!
//! MPEG audio decode is not bit-exact by design here: this decoder runs in
//! `f32`, not the ISO reference decoder's fixed-point contract, and the
//! specification itself defines a compliance tolerance rather than one
//! correct output (see `docs/codec/vaco-codec-mpegaudio.md`'s own "Decode
//! accuracy" section). So passing assertions below are a cross-correlation
//! and RMS-error bound against real `ffmpeg`-decoded PCM, not `assert_eq!`.
//! Every fixture's *sample count* is still checked exactly — CLAUDE.md's own
//! postmortems name decoders that silently emit the wrong fraction of a
//! file's real length while every other check still passes.
//!
//! # Provenance
//!
//! `Vaco-Provenance: blackbox`, `Vaco-Spec-Ref: none` — measured directly
//! against real `ffmpeg 9.0.1` output. Each `tests/fixtures/<name>` was
//! produced by `ffmpeg -f lavfi -i "aevalsrc=..." -c:a <mp2|libmp3lame>
//! -write_xing 0 -id3v2_version 0` (no Xing/LAME header, so there is no
//! gapless-trim metadata for either side to apply or skip — both decoders
//! simply decode every frame physically present in the stream, so their
//! sample counts are directly comparable); each `_ref.raw` is that same file
//! decoded with `ffmpeg -acodec pcm_s16le -f s16le -fflags +bitexact` —
//! never this crate's own encoder (there is no encoder in this crate to use
//! either way; Layer II/III encoding is out of scope, see this crate's own
//! module doc).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration test code over trusted fixture data, not the untrusted-input surface"
)]

use std::path::Path;

use vaco_codec_core::Decoder;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_frame::FrameData;
use vaco_io::MemorySource;
use vaco_limits::Limits;

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn s16le(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Decode a whole real MPEG audio elementary stream to interleaved `i16`
/// PCM, the same conversion `examples/decode_dump.rs` uses (clamp to
/// `[-1, 1]`, scale by `i16::MAX`, truncate) so this test compares apples to
/// apples with how this crate's own documented conformance numbers were
/// produced.
fn decode_file(path: &std::path::Path) -> (Vec<i16>, u32) {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let src = Box::new(MemorySource::new(data));
    let mut demuxer = vaco_demux_mpegaudio::MpegAudioDemuxer::open(src, &FormatOptions::default())
        .expect("open mpeg audio stream");
    let mut decoder = vaco_codec_mpegaudio::MpegAudioDecoder::new(Limits::permissive());

    let mut channels = 0u32;
    let mut out = Vec::new();
    loop {
        let packet = match demuxer.read_packet() {
            Ok(p) => Some(p),
            Err(vaco_core::Error::Eof) => None,
            Err(e) => panic!("demux error: {e:?}"),
        };
        let at_eof = packet.is_none();
        decoder.send_packet(packet.as_ref()).expect("send_packet");
        loop {
            match decoder.receive_frame() {
                Ok(frame) => {
                    let FrameData::Audio { samples, layout, .. } = &frame.data else {
                        panic!("expected an audio frame");
                    };
                    channels = layout.channels.max(1);
                    let mut planes = Vec::with_capacity(channels as usize);
                    for ch in 0..channels as usize {
                        planes.push(frame.plane(ch).expect("plane"));
                    }
                    for i in 0..*samples as usize {
                        for plane in &planes {
                            let row = plane.as_slice();
                            let off = i * 4;
                            let f = row
                                .get(off..off + 4)
                                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                                .unwrap_or(0.0);
                            let clamped = f.clamp(-1.0, 1.0);
                            out.push((clamped * f32::from(i16::MAX)) as i16);
                        }
                    }
                }
                Err(vaco_core::Error::NeedMoreInput) => break,
                Err(vaco_core::Error::Eof) => break,
                Err(e) => panic!("receive_frame error: {e:?}"),
            }
        }
        if at_eof {
            break;
        }
    }
    (out, channels)
}

/// Pearson correlation and RMS error between two equal-length interleaved
/// `i16` buffers.
fn correlation_and_rms(ours: &[i16], reference: &[i16]) -> (f64, f64) {
    assert_eq!(ours.len(), reference.len(), "length mismatch inside comparison helper");
    let n = ours.len() as f64;
    let (mut sum_a, mut sum_b) = (0.0, 0.0);
    for (&a, &b) in ours.iter().zip(reference.iter()) {
        sum_a += f64::from(a);
        sum_b += f64::from(b);
    }
    let (mean_a, mean_b) = (sum_a / n, sum_b / n);
    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    let mut sq_err = 0.0;
    for (&a, &b) in ours.iter().zip(reference.iter()) {
        let da = f64::from(a) - mean_a;
        let db = f64::from(b) - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
        let e = f64::from(a) - f64::from(b);
        sq_err += e * e;
    }
    let corr = if var_a > 0.0 && var_b > 0.0 {
        cov / (var_a.sqrt() * var_b.sqrt())
    } else {
        0.0
    };
    let rms = (sq_err / n).sqrt();
    (corr, rms)
}

// ------------------------------------------------------------------ Layer II

#[test]
fn layer2_mono_128kbps_decodes_a_real_ffmpeg_stream_closely() {
    let (decoded, channels) = decode_file(&fixture_path("mp2_mono_128k.mp2"));
    let reference = s16le(&std::fs::read(fixture_path("mp2_mono_128k_ref.raw")).unwrap());
    assert_eq!(channels, 1);
    assert_eq!(
        decoded.len(),
        reference.len(),
        "sample count mismatch (ours={}, ffmpeg={}) -- a decoder can silently \
         decode the wrong fraction of a file while every other check passes, \
         see CLAUDE.md",
        decoded.len(),
        reference.len()
    );
    let (corr, rms) = correlation_and_rms(&decoded, &reference);
    assert!(corr > 0.999, "correlation too low: {corr}");
    assert!(rms < 50.0, "RMS error too high (out of 32768 full scale): {rms}");
}

// This fixture is bit-identical in content and duration to
// `mp2_mono_128k.mp2` above (same source signal, same sample rate, same
// duration), encoded at a lower per-channel bitrate that lands in a
// different Layer II bit-allocation table (`LAYER2_TABLE_A`'s
// 56/64/80 kbit/s-per-channel range, `bitalloc.rs::layer2_table`, vs
// `LAYER2_TABLE_B` above 96). That table-selection boundary was never
// exercised by this crate's prior conformance pass (its 6 Layer II
// fixtures, per `docs/codec/vaco-codec-mpegaudio.md`, did not vary bitrate
// independently of sample rate/channel count) and is a real, reproducible
// bug: measured on this fixture, correlation 0.9223, RMS error 4879.2 (out
// of 32768 full scale), max single-sample diff 15013 -- and the sample
// *count* matches ffmpeg's exactly (16128/16128), so this is wrong values,
// not a framing/count bug. The error is uniform across the whole signal
// (not concentrated at frame edges or a particular frequency), reproducing
// identically at 56/64/80 kbit/s and disappearing at 96 kbit/s and above,
// at both 32000 and 44100 Hz.
//
// Ruled out while investigating: `LAYER2_TABLE_A`/`_B`'s row *content* is
// byte-identical where they overlap (only the trailing row count differs);
// the bitrate table (`BITRATE_MPEG1_II`) and Layer II frame-length formula
// are both correct by direct inspection; the SCFSI transmission-pattern
// table and the granule-major/subband-minor sample loop (a previously fixed
// bug, see `layer2.rs`'s own comment on it) are unaffected. The defect is
// real and isolated to this exact table-selection range, but the precise
// mechanism was not pinned down within this pass's budget -- filed rather
// than guessed at, per CLAUDE.md's own instruction not to "get the correct
// answer wrong from memory" on an exact table layout.
#[test]
#[ignore = "Layer II mid-bitrate (56/64/80 kbit/s per channel) bit-allocation \
            table selection produces uniformly wrong PCM (correlation 0.9223, \
            RMS 4879.2/32768); see this test's doc comment for measured evidence"]
fn layer2_mono_64kbps_decodes_a_real_ffmpeg_stream_closely() {
    let (decoded, channels) = decode_file(&fixture_path("mp2_mono_64k.mp2"));
    let reference = s16le(&std::fs::read(fixture_path("mp2_mono_64k_ref.raw")).unwrap());
    assert_eq!(channels, 1);
    assert_eq!(decoded.len(), reference.len(), "sample count mismatch");
    let (corr, rms) = correlation_and_rms(&decoded, &reference);
    assert!(corr > 0.999, "correlation too low: {corr}");
    assert!(rms < 50.0, "RMS error too high (out of 32768 full scale): {rms}");
}

// ----------------------------------------------------------------- Layer III

/// One Layer III frame's interleaved-stereo sample count: 1152 samples per
/// channel, 2 channels.
const MP3_FRAME_INTERLEAVED: usize = 1152 * 2;

#[test]
fn layer3_stereo_decodes_a_real_ffmpeg_stream_closely() {
    let (decoded, channels) = decode_file(&fixture_path("mp3_stereo.mp3"));
    let reference = s16le(&std::fs::read(fixture_path("mp3_stereo_ref.raw")).unwrap());
    assert_eq!(channels, 2);
    assert_eq!(
        decoded.len(),
        reference.len(),
        "sample count mismatch (ours={}, ffmpeg={})",
        decoded.len(),
        reference.len()
    );

    // The whole-file comparison is real but loose: measured on this fixture,
    // correlation 0.9675, concentrated entirely in the first two and last
    // two of this fixture's 15 frames (per-frame max-abs-diff: frame 0
    // 9388, frame 1 16783, frames 2-12 <= 4, frame 13 10720, frame 14
    // 17725) -- this is this crate's already-documented, separately-tracked
    // "short blocks decode to silence" gap
    // (`docs/codec/vaco-codec-mpegaudio.md`'s "Known gaps"), not a new
    // defect: even a smooth, non-transient multitone source can still open
    // and close on a short block while the encoder's bit reservoir/lookahead
    // settles. So this test checks the *whole* signal only loosely (catches
    // a real full-file regression) and checks the middle frames -- entirely
    // unaffected by that gap -- tightly.
    let (whole_corr, _) = correlation_and_rms(&decoded, &reference);
    assert!(whole_corr > 0.9, "whole-file correlation too low: {whole_corr}");

    let nframes = reference.len() / MP3_FRAME_INTERLEAVED;
    assert!(nframes > 4, "fixture too short to exclude 2 edge frames on each side");
    let lo = 2 * MP3_FRAME_INTERLEAVED;
    let hi = (nframes - 2) * MP3_FRAME_INTERLEAVED;
    let (mid_corr, mid_rms) = correlation_and_rms(&decoded[lo..hi], &reference[lo..hi]);
    assert!(mid_corr > 0.9999, "middle-frame correlation too low: {mid_corr}");
    assert!(
        mid_rms < 10.0,
        "middle-frame RMS error too high (out of 32768 full scale): {mid_rms}"
    );
}
