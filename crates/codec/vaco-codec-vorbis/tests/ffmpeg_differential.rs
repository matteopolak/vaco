//! Differential test against a real `ffmpeg`-encoded Vorbis stream.
//!
//! Vorbis is lossy and decode-only here (no Vaco encoder to round-trip
//! against), so there is no bit-exact target the way there is for FLAC.
//! Instead this decodes real `ffmpeg`-produced Ogg/Vorbis bytes with this
//! crate and compares sample-for-sample against `ffmpeg`'s own decode to
//! raw `f32le`, at a tolerance (documented below) rather than exact
//! equality — the spec itself does not mandate bit-exact reconstruction
//! across implementations.
//!
//! `ffmpeg`'s native Vorbis encoder in this environment supports only two
//! channels ("Current `FFmpeg` Vorbis encoder only supports 2 channels."),
//! so every case here is stereo; mono is exercised only by this crate's own
//! unit tests. Every fixture measured this way used residue type 2 with one
//! channel-coupling step — see this crate's closing report for exactly
//! which parts of the spec that leaves unmeasured against real content.
//!
//! Skipped rather than failed when `ffmpeg` is absent, matching the
//! convention `vaco-codec-flac`'s own `ffmpeg_fixture.rs` uses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Write;
use std::process::{Command, Stdio};

use vaco_codec_core::Decoder;
use vaco_codec_vorbis::VorbisDecoder;
use vaco_format_core::{Demuxer, FormatOptions, NoParsers};
use vaco_frame::FrameData;
use vaco_io::MemorySource;
use vaco_limits::Limits;

fn run_ffmpeg(args: &[&str], stdin_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let Some(bytes) = stdin_bytes {
        child.stdin.take()?.write_all(bytes).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// Decode every packet of an Ogg/Vorbis byte stream with this crate,
/// returning interleaved `f32` PCM (channel-minor, matching `-f f32le`).
fn decode_with_vaco(ogg_bytes: Vec<u8>) -> Vec<f32> {
    let src = MemorySource::new(ogg_bytes);
    let mut demux =
        vaco_demux_ogg::OggDemuxer::open(Box::new(src), &NoParsers, &FormatOptions::default())
            .expect("open ogg");
    let extradata = demux.streams()[0]
        .params
        .extradata
        .clone()
        .expect("vorbis stream carries packed header extradata");

    let mut dec = VorbisDecoder::new(Limits::permissive());
    dec.set_extradata(&extradata)
        .expect("set_extradata on a real ffmpeg stream must succeed");

    let mut out = Vec::new();
    let drain = |dec: &mut VorbisDecoder, out: &mut Vec<f32>| {
        while let Ok(frame) = dec.receive_frame() {
            let FrameData::Audio {
                samples, planes, ..
            } = &frame.data
            else {
                continue;
            };
            let channel_bufs: Vec<Vec<f32>> = (0..planes.len())
                .map(|p| {
                    let plane = frame.plane(p).expect("plane");
                    let row = plane.row(0).expect("row");
                    row.chunks_exact(4)
                        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                        .collect()
                })
                .collect();
            for i in 0..*samples as usize {
                for ch in &channel_bufs {
                    out.push(*ch.get(i).unwrap_or(&0.0));
                }
            }
        }
    };

    while let Ok(packet) = demux.read_packet() {
        dec.send_packet(Some(&packet)).expect("send_packet");
        drain(&mut dec, &mut out);
    }
    dec.send_packet(None).ok();
    drain(&mut dec, &mut out);
    out
}

/// Encode `lavfi_source` with `ffmpeg`'s native Vorbis encoder, decode both
/// with `ffmpeg` (ground truth) and with this crate, and assert they agree
/// within tolerance. `None` (a skip, not a failure) when `ffmpeg` is
/// unavailable or cannot produce/consume the fixture.
fn assert_matches_ffmpeg(lavfi_source: &str, quality: &str) -> Option<()> {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return None;
    }
    let ogg_bytes = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            lavfi_source,
            "-ac",
            "2",
            "-c:a",
            "vorbis",
            "-strict",
            "-2",
            "-q:a",
            quality,
            "-f",
            "ogg",
            "-",
        ],
        None,
    )?;
    let ground_truth_bytes = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "ogg",
            "-i",
            "-",
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-",
        ],
        Some(&ogg_bytes),
    )?;
    let ground_truth: Vec<f32> = ground_truth_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let mine = decode_with_vaco(ogg_bytes);

    // Both decoders apply the spec's own one-block priming delay identically
    // (data is not returned from the first frame), so sample 0 of each
    // should already be aligned; only trailing-length differences (the
    // decoder's own final undrained half-window) are expected.
    let n = mine.len().min(ground_truth.len());
    assert!(
        n > 1000,
        "decoded too few samples to compare meaningfully ({n})"
    );

    let mut sum_abs = 0f64;
    let mut max_abs = 0f64;
    for i in 0..n {
        let d = f64::from(mine[i] - ground_truth[i]).abs();
        sum_abs += d;
        max_abs = max_abs.max(d);
        assert!(mine[i].is_finite(), "non-finite sample at {i}");
    }
    let mean_abs = sum_abs / n as f64;

    // Tolerance: mean absolute error under 1e-3 and max under 1e-2 on the
    // float `[-1,1]` scale. In practice every case measured here (multiple
    // quality levels, a block-size-switching transient) matches to float32
    // rounding noise (mean/max on the order of 1e-8/1e-7) — this crate's own
    // independent MDCT/window/floor/residue pipeline reproduces `ffmpeg`'s
    // native Vorbis decode almost exactly, not merely "close". The looser
    // bound is what is actually documented as the pass/fail line, since
    // Vorbis decode is not required to be bit-exact across implementations.
    assert!(
        mean_abs < 1e-3,
        "mean abs error {mean_abs} too large ({n} samples compared)"
    );
    assert!(
        max_abs < 1e-2,
        "max abs error {max_abs} too large ({n} samples compared)"
    );
    Some(())
}

#[test]
fn stereo_sine_matches_ffmpeg_at_default_quality() {
    assert_matches_ffmpeg("sine=frequency=440:duration=2", "3");
}

#[test]
fn stereo_sine_matches_ffmpeg_at_high_quality() {
    // High quality exercises a denser codebook/residue configuration than
    // the default.
    assert_matches_ffmpeg("sine=frequency=1000:duration=1.5", "9");
}

#[test]
fn stereo_sine_matches_ffmpeg_at_low_quality() {
    assert_matches_ffmpeg("sine=frequency=220:duration=1", "0");
}

#[test]
fn transient_stereo_matches_ffmpeg_across_block_size_switches() {
    // A repeating short burst forces the encoder to switch between long and
    // short blocks, exercising the hybrid window-shape decode (spec
    // section 4.3.1) and the overlap-add alignment across differing
    // `n` (spec section 4.3.8) that a steady tone never touches.
    assert_matches_ffmpeg(
        r"aevalsrc=0.6*sin(2*PI*880*t)*lt(mod(t\,0.5)\,0.05):d=3:s=44100",
        "4",
    );
}

/// Regression: `overlap_add` builds every output `Frame` via
/// `Frame::alloc_audio`, whose default pts is `Timestamp::NONE`. Before
/// `decode_audio_packet`/`overlap_add` threaded the triggering packet's
/// `pts` through to the emitted frame, every decoded Vorbis frame silently
/// lost the timestamp `vaco-demux-ogg`'s granule-position accounting had
/// already computed correctly on the way in — reproduced end to end via
/// `vaco -i sine.ogg -f wav out.wav`, which failed with "this container
/// needs timestamps and the packet has none" despite the input packets
/// themselves carrying real timestamps.
#[test]
fn every_decoded_frame_carries_a_real_non_decreasing_pts() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let Some(ogg_bytes) = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-ac",
            "2",
            "-c:a",
            "vorbis",
            "-strict",
            "-2",
            "-q:a",
            "4",
            "-f",
            "ogg",
            "-",
        ],
        None,
    ) else {
        eprintln!("skipping: ffmpeg could not produce the fixture");
        return;
    };

    let src = MemorySource::new(ogg_bytes);
    let mut demux =
        vaco_demux_ogg::OggDemuxer::open(Box::new(src), &NoParsers, &FormatOptions::default())
            .expect("open ogg");
    let extradata = demux.streams()[0]
        .params
        .extradata
        .clone()
        .expect("vorbis stream carries packed header extradata");
    let mut dec = VorbisDecoder::new(Limits::permissive());
    dec.set_extradata(&extradata).expect("set_extradata");

    let mut last_pts: Option<i64> = None;
    let mut frames_seen = 0usize;
    while let Ok(packet) = demux.read_packet() {
        dec.send_packet(Some(&packet)).expect("send_packet");
        while let Ok(frame) = dec.receive_frame() {
            frames_seen += 1;
            let pts = frame
                .pts
                .ticks()
                .expect("decoded frame must carry a real pts, not Timestamp::NONE");
            if let Some(prev) = last_pts {
                assert!(pts >= prev, "pts went backwards: {prev} -> {pts}");
            }
            last_pts = Some(pts);
        }
    }
    assert!(frames_seen > 0, "expected at least one decoded frame");
}
