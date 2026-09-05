//! Cross-check both directions of the FLAC boundary against the real ffmpeg
//! binary. ffmpeg-produced FLAC must decode to identical samples here, and
//! our encoded FLAC must decode to identical samples in ffmpeg. FLAC is
//! lossless, so nothing less than exact equality is a pass.
//!
//! Skipped rather than failed when `ffmpeg` is absent, matching the
//! convention `vaco-codec-core`'s own `params.rs` test uses: CI has it, a
//! contributor's machine may not, and a test that cannot run is not a test
//! that failed.
//!
//! Vaco-Spec-Ref: ffmpeg-flac-fixture-probe native `.flac` framing
//! (metadata-block-header walk to find where frames start) used to split
//! this crate's own already-demuxed-packet decode path from a raw
//! reference file, for test purposes only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::process::{Command, Stdio};

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Encoder;
use vaco_codec_flac::FlacEncoder;
use vaco_codec_flac::claxon_boundary::decode_packet;
use vaco_codec_flac::streaminfo::find_streaminfo_block;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_sampfmt::SampleFmt;

/// Walk past every metadata block after the `"fLaC"` marker (STREAMINFO,
/// and whatever else ffmpeg wrote — a Vorbis comment block, typically) and
/// return the byte offset the first frame starts at.
fn frame_data_offset(flac_bytes: &[u8]) -> Option<usize> {
    let mut offset = flac_bytes.get(..4).filter(|m| *m == b"fLaC")?.len();
    loop {
        let header = *flac_bytes.get(offset)?;
        let b1 = u32::from(*flac_bytes.get(offset + 1)?);
        let b2 = u32::from(*flac_bytes.get(offset + 2)?);
        let b3 = u32::from(*flac_bytes.get(offset + 3)?);
        let len = ((b1 << 16) | (b2 << 8) | b3) as usize;
        offset = offset.checked_add(4)?.checked_add(len)?;
        if header & 0x80 != 0 {
            return Some(offset);
        }
    }
}

fn run_ffmpeg(args: &[&str], stdin_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    use std::io::Write;
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
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

fn mono_s16p_frame(samples: &[i16]) -> Frame {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        SampleFmt::S16P,
        ChannelLayout::MONO,
        u32::try_from(samples.len()).unwrap_or(0),
        8_000,
    )
    .expect("allocate trusted test frame");
    {
        let mut planes = frame.planes_mut();
        if let Some(row) = planes.first_mut().and_then(|plane| plane.row_mut(0)) {
            for (dst, sample) in row.chunks_exact_mut(2).zip(samples) {
                dst.copy_from_slice(&sample.to_ne_bytes());
            }
        }
    }
    frame
}

fn encode_our_flac(samples: &[i16]) -> Vec<u8> {
    let frame = mono_s16p_frame(samples);
    let mut enc = FlacEncoder::new(Limits::permissive());
    enc.send_frame(Some(&frame)).expect("encode test frame");
    enc.send_frame(None).expect("drain encoder");
    let mut bytes = enc.extradata();
    while let Ok(packet) = enc.receive_packet() {
        bytes.extend_from_slice(packet.payload());
    }
    bytes
}

#[test]
fn decodes_a_real_ffmpeg_flac_stream_exactly() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }

    let Some(flac_bytes) = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1:sample_rate=8000",
            "-ac",
            "1",
            "-c:a",
            "flac",
            "-f",
            "flac",
            "-",
        ],
        None,
    ) else {
        eprintln!("skipping: ffmpeg could not produce a FLAC fixture");
        return;
    };

    let Some(ground_truth) = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "flac",
            "-i",
            "-",
            "-f",
            "s16le",
            "-",
        ],
        Some(&flac_bytes),
    ) else {
        eprintln!("skipping: ffmpeg could not decode its own FLAC fixture");
        return;
    };

    let streaminfo = find_streaminfo_block(&flac_bytes).expect("STREAMINFO present in own output");
    let frame_start = frame_data_offset(&flac_bytes).expect("frame data present");
    let frame_bytes = flac_bytes.get(frame_start..).expect("frame data slice");

    let decoded = decode_packet(&streaminfo, frame_bytes).expect("decode via claxon");

    let want: Vec<i32> = ground_truth
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
        .collect();

    assert_eq!(
        decoded.interleaved.len(),
        want.len(),
        "sample count must match ffmpeg's own decode exactly"
    );
    assert_eq!(
        decoded.interleaved, want,
        "decoded samples must match ffmpeg's own `-f s16le` dump at zero tolerance"
    );
}

#[test]
fn ffmpeg_decodes_our_dispatched_lpc_flac_exactly() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }

    // One FLAC-sized block forces every LPC candidate order through the
    // runtime-dispatched autocorrelation path before the encoder chooses its
    // smallest valid subframe.
    let samples: Vec<i16> = (0..4096)
        .map(|i| {
            let t = f64::from(i) / 8_000.0;
            (12_000.0 * (2.0 * std::f64::consts::PI * 439.0 * t).sin()) as i16
        })
        .collect();
    let flac_bytes = encode_our_flac(&samples);
    assert!(
        flac_bytes.starts_with(b"fLaC"),
        "encoder produced FLAC header"
    );

    let decoded = run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "flac",
            "-i",
            "-",
            "-f",
            "s16le",
            "-",
        ],
        Some(&flac_bytes),
    )
    .expect("ffmpeg decodes this crate's FLAC stream");
    let expected: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    assert_eq!(decoded, expected, "ffmpeg must reproduce every PCM sample");
}
