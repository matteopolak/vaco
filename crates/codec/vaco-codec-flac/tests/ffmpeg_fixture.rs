//! Cross-check the `claxon`-backed decode boundary against a real,
//! ffmpeg-produced FLAC stream, comparing decoded samples to ffmpeg's own
//! `-f s16le` ground truth at zero tolerance (FLAC is lossless, so nothing
//! less than exact equality is a pass).
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

use vaco_codec_flac::claxon_boundary::decode_packet;
use vaco_codec_flac::streaminfo::find_streaminfo_block;

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
