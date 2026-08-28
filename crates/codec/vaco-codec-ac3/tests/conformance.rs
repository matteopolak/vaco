//! The decode-accuracy matrix: decode every committed fixture
//! and measure PCM error against `ffmpeg`'s own decode, rather than assert a
//! bit-exactness this crate's bit-allocation model (see
//! `crate::tables_bitalloc`'s doc comment) cannot currently back up.
//!
//! Fixtures are four real frames each (`-frames:a 4`), generated 2026-08-27
//! with ffmpeg 8.1:
//!
//! ```sh
//! ffmpeg -f lavfi -i "sine=frequency=440:duration=1:sample_rate=48000" \
//!   [-af aformat=channel_layouts=<layout>] -c:a <ac3|eac3> -b:a <rate> \
//!   [-dialnorm <n>] -frames:a 4 fixture.ac3
//! ffmpeg -i fixture.ac3 -f f32le -acodec pcm_f32le fixture.ref.f32le
//! ```
//!
//! `small_ac3.ac3` is the pre-existing fixture `vaco-format-spdif` also
//! uses, reused here rather than duplicated.
//!
//! This test never fails on measured error alone — the point is to report
//! it truthfully rather than assert a pass/fail threshold this crate cannot
//! currently justify. It does fail on a panic, a decode error, or
//! `NaN`/non-finite output, which would indicate the pipeline is broken
//! rather than merely inaccurate.

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_ac3::{DecodeOptions, StreamState, decode_frame};
use vaco_format_ac3::syncinfo;

struct Fixture {
    name: &'static str,
    ac3: &'static [u8],
    reference_f32le: &'static [u8],
    drc: bool,
}

macro_rules! fixture {
    ($name:literal, $ac3:literal, $ref:literal) => {
        Fixture {
            name: $name,
            ac3: include_bytes!(concat!("fixtures/", $ac3)),
            reference_f32le: include_bytes!(concat!("fixtures/", $ref)),
            drc: false,
        }
    };
}

const FIXTURES: &[Fixture] = &[
    fixture!(
        "ac3 mono 192k",
        "fx_ac3_mono_192k.ac3",
        "fx_ac3_mono_192k.ref.f32le"
    ),
    fixture!("ac3 stereo 192k", "small_ac3.ac3", "small_ac3.ref.f32le"),
    fixture!(
        "ac3 stereo 384k",
        "fx_ac3_stereo_384k.ac3",
        "fx_ac3_stereo_384k.ref.f32le"
    ),
    fixture!(
        "ac3 5.1 448k",
        "fx_ac3_51_448k.ac3",
        "fx_ac3_51_448k.ref.f32le"
    ),
    fixture!(
        "ac3 stereo dialnorm=-20",
        "fx_ac3_stereo_dialnormN20.ac3",
        "fx_ac3_stereo_dialnormN20.ref.f32le"
    ),
    fixture!(
        "ac3 stereo 44.1kHz",
        "fx_ac3_stereo_44100.ac3",
        "fx_ac3_stereo_44100.ref.f32le"
    ),
];

/// Split a whole raw AC-3/E-AC-3 buffer into per-syncframe payloads.
fn split_frames(mut data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while let Some(info) = syncinfo::parse(data) {
        let Some((frame, rest)) = data.split_at_checked(info.frame_size) else {
            break;
        };
        out.push(frame);
        data = rest;
    }
    out
}

/// `(max_abs, rms)` between a decoded, per-channel-planar signal and an
/// interleaved reference, matched channel by channel.
fn measure_error(decoded: &[Vec<f32>], reference_interleaved: &[u8]) -> (f32, f64) {
    let channels = decoded.len().max(1);
    let ref_samples: Vec<f32> = reference_interleaved
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut max_abs = 0f32;
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    for (ch, samples) in decoded.iter().enumerate() {
        for (i, &s) in samples.iter().enumerate() {
            let Some(&r) = ref_samples.get(i * channels + ch) else {
                continue;
            };
            let diff = (s - r).abs();
            max_abs = max_abs.max(diff);
            sum_sq += f64::from(diff) * f64::from(diff);
            n += 1;
        }
    }
    let rms = if n > 0 {
        (sum_sq / n as f64).sqrt()
    } else {
        0.0
    };
    (max_abs, rms)
}

#[test]
fn decode_accuracy_matrix() {
    println!(
        "\n{:<28} {:>10} {:>12} {:>10}",
        "fixture", "frames", "max_abs_err", "rms_err"
    );
    for fx in FIXTURES {
        let frames = split_frames(fx.ac3);
        assert!(!frames.is_empty(), "{}: no frames parsed", fx.name);

        let mut state = StreamState::new();
        let opts = DecodeOptions { apply_drc: fx.drc };
        let mut per_channel: Vec<Vec<f32>> = Vec::new();
        for payload in &frames {
            let decoded = decode_frame(payload, &mut state, &opts)
                .unwrap_or_else(|_| panic!("{}: decode_frame failed", fx.name));
            if per_channel.is_empty() {
                per_channel =
                    vec![Vec::new(); decoded.channels.len() + usize::from(decoded.lfe.is_some())];
            }
            for (ch, samples) in decoded.channels.iter().enumerate() {
                if let Some(slot) = per_channel.get_mut(ch) {
                    slot.extend_from_slice(samples);
                }
            }
            if let Some(lfe) = &decoded.lfe {
                let idx = per_channel.len() - 1;
                if let Some(slot) = per_channel.get_mut(idx) {
                    slot.extend_from_slice(lfe);
                }
            }
        }

        for (ch, samples) in per_channel.iter().enumerate() {
            for &s in samples {
                assert!(
                    s.is_finite(),
                    "{}: channel {ch} produced a non-finite sample",
                    fx.name
                );
            }
        }

        let (max_abs, rms) = measure_error(&per_channel, fx.reference_f32le);
        println!(
            "{:<28} {:>10} {:>12.6} {:>10.6}",
            fx.name,
            frames.len(),
            max_abs,
            rms
        );
    }
}

#[test]
fn eac3_frames_parse_structurally_without_the_decode_feature() {
    // Without `patent-unverified-eac3-decode`, `decode_frame` correctly
    // refuses an E-AC-3 payload (`FrameKind::Eac3`) rather than
    // misinterpreting it as AC-3 — verifies the gate in `decode::decode_frame`
    // itself, independent of whether the feature is enabled for this run.
    let data = include_bytes!("fixtures/fx_eac3_stereo_192k.eac3");
    let frames = split_frames(data);
    assert!(!frames.is_empty());
    let mut state = StreamState::new();
    let opts = DecodeOptions::default();
    for payload in &frames {
        let result = decode_frame(payload, &mut state, &opts);
        #[cfg(not(feature = "patent-unverified-eac3-decode"))]
        assert!(result.is_err(), "an E-AC-3 frame must not decode as AC-3");
        #[cfg(feature = "patent-unverified-eac3-decode")]
        let _ = result;
    }
}
