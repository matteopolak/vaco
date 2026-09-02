//! Frame-threading conformance for VP9 (issue #328, `C-32c`): every
//! committed fixture decodes to byte-identical [`Frame`]s at `-threads`
//! 1, 2, 4 and 8, driven directly through [`Vp9Decoder`] (no `ffmpeg`, no
//! process spawn) — the determinism half of the acceptance criterion,
//! checked on every `cargo test`, not just `--ignored`.
//!
//! [`decode_matches_ffmpeg_on_the_committed_vectors`] is the black-box-oracle
//! half (D6/D7: `ffmpeg` itself, never its source): it shells out to the
//! system `ffmpeg` binary, so it is `#[ignore]`d and run explicitly.
//!
//! ## Fixtures
//!
//! Both committed `.ivf` files were produced locally with the system
//! `ffmpeg`'s `libvpx-vp9` encoder (never read as source — D6/D7/D17
//! restrict that to the *decoder*, not to using the real binary as an
//! encoder/oracle):
//!
//! - `vp9_two_tile_columns_512x384.ivf`: `ffmpeg -f lavfi -i
//!   testsrc2=size=512x384:rate=10:duration=1.5 -pix_fmt yuv420p -c:v
//!   libvpx-vp9 -tile-columns 2 -frame-parallel 1 -row-mt 1 -auto-alt-ref 1
//!   -lag-in-frames 16 -g 20 -b:v 800k`. Exercises multiple tile columns
//!   and inter frames end-to-end (the unit test in `src/decode.rs` only
//!   covers one key frame's first-tile-column-vs-second-tile-column pixel
//!   boundary; this exercises the same geometry across a whole stream that
//!   also references prior frames).
//! - `vp9_altref_invisible_frames.ivf`: a 2-pass `libvpx-vp9` encode
//!   (`ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=5
//!   -pix_fmt yuv420p -c:v libvpx-vp9 -deadline good -cpu-used 1
//!   -auto-alt-ref 1 -lag-in-frames 25 -arnr-maxframes 15 -arnr-strength 6
//!   -g 250 -b:v 200k -pass 1/2`) chosen specifically because two-pass mode
//!   reliably makes `libvpx` emit real invisible alt-ref frames
//!   (`show_frame = 0`, confirmed by hand-parsing the VP9 uncompressed
//!   header's bits directly out of each IVF packet: 10 of this file's 125
//!   coded frames have `show_frame = 0`, none use `show_existing_frame`).
//!   This is exactly the case the frame-threading split has to get right
//!   without a single-frame test noticing: an invisible frame must still
//!   be fully reconstructed (later frames' reference-frame store depends on
//!   its pixels) but must never reach [`Vp9Decoder::receive_frame`] as
//!   output — a regression here is silent in a frame *count* check on any
//!   fixture that lacks one, which is why the original single-tile-column
//!   fixture above does not by itself cover it.
//!
//! fuzz-crate: vaco-codec-vp9 (this is `cargo test`, not the fuzzer, but
//! lives beside it for the same "real bitstream, not synthesised" reason).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test: a panic here is a test failure by design, and slicing on \
              offsets this same file just computed from the vector's own reported geometry \
              is the readable form of a bounds check that would otherwise just be reasserting \
              the arithmetic two lines up"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use vaco_codec_core::Decoder;
use vaco_codec_vp9::Vp9Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Split an IVF file into its raw per-frame VP9 payloads. Trivial and
/// test-only, so a hand-rolled reader is preferable to pulling in a demuxer
/// crate (there is no `vaco-demux-ivf` in this workspace, and a codec crate
/// reaching for a demux dependency for a test harness would be exactly the
/// layering shortcut D14.1 exists to prevent) — copied verbatim from
/// `vaco-codec-vp8`'s own `tests/conformance.rs`, which established this
/// pattern for issue #301.
fn ivf_frame_payloads(bytes: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    if bytes.len() < 32 || bytes.get(0..4) != Some(b"DKIF".as_slice()) {
        return frames;
    }
    let header_len = bytes
        .get(6..8)
        .map_or(32, |b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .max(32);
    let mut off = header_len;
    while let Some(hdr) = bytes.get(off..off + 12) {
        let size = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        let payload_start = off + 12;
        let Some(payload) = bytes.get(payload_start..payload_start + size) else { break };
        frames.push(payload);
        off = payload_start + size;
    }
    frames
}

/// One shown frame's dimensions and its Y/U/V bytes, tightly packed in
/// `ffmpeg -f rawvideo -pix_fmt yuv420p` order (whole Y plane, then whole U,
/// then whole V).
struct DecodedFrame {
    width: u32,
    height: u32,
    yuv: Vec<u8>,
}

fn drain_ready(dec: &mut Vp9Decoder, out: &mut Vec<DecodedFrame>) {
    while let Ok(frame) = dec.receive_frame() {
        let Some((width, height)) = frame.dimensions() else { continue };
        let mut yuv = Vec::new();
        for idx in 0..3 {
            let Some(plane) = frame.plane(idx) else { continue };
            for r in 0..plane.rows() {
                yuv.extend_from_slice(plane.row(r).unwrap_or(&[]));
            }
        }
        out.push(DecodedFrame { width, height, yuv });
    }
}

/// Decode every packet of an IVF file with this crate's own decoder, one
/// entry per *shown* frame (an invisible alt-ref frame contributes nothing,
/// matching `ffmpeg`'s own raw dump — see this module's doc for why that is
/// exactly the behaviour under test).
fn decode_all(dec: &mut Vp9Decoder, ivf_bytes: &[u8]) -> Vec<DecodedFrame> {
    let mut budget = Budget::new(Limits::default());
    let mut out = Vec::new();
    for payload in ivf_frame_payloads(ivf_bytes) {
        let Ok(packet) = Packet::from_slice(&mut budget, payload) else { continue };
        if dec.send_packet(Some(&packet)).is_err() {
            continue;
        }
        drain_ready(dec, &mut out);
    }
    // End of stream: a frame-threaded decoder may still be holding
    // pictures it accepted but has not yet had to collect (only
    // `max_in_flight` forces that during decode) — `send_packet(None)` is
    // this trait's own drain signal. Skipping it would silently lose
    // exactly those pictures rather than decode them wrong (the same trap
    // `vaco-codec-vp8`'s test harness names and had already hit once).
    let _ = dec.send_packet(None);
    drain_ready(dec, &mut out);
    out
}

fn fixtures_dir() -> PathBuf {
    std::env::var("VACO_VP9_CONFORMANCE_DIR").map_or_else(
        |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vp9"),
        PathBuf::from,
    )
}

fn fixture_paths() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ivf"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .ivf vectors found in {}", dir.display());
    entries
}

/// Frame threading (issue #328): every committed fixture decodes to
/// byte-identical `Frame`s at `-threads` 1, 2, 4 and 8. Covers both a
/// multi-tile-column, multi-frame stream and a stream with real invisible
/// alt-ref frames (`show_frame = 0`) — see this module's doc for why the
/// latter matters specifically for the "collect emits only shown frames"
/// half of the split.
#[test]
fn threads_are_byte_identical() {
    for path in &fixture_paths() {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut baseline: Option<Vec<DecodedFrame>> = None;
        for threads in [1usize, 2, 4, 8] {
            let mut dec = Vp9Decoder::new(Limits::default());
            let granted = dec.set_thread_count(threads);
            let frames = decode_all(&mut dec, &bytes);
            assert!(!frames.is_empty(), "{}: threads={threads} decoded zero frames", path.display());
            if let Some(base) = &baseline {
                assert_eq!(
                    base.len(),
                    frames.len(),
                    "{}: threads={threads} (granted {granted:?}) produced a different frame count than threads=1",
                    path.display()
                );
                for (i, (b, f)) in base.iter().zip(frames.iter()).enumerate() {
                    assert_eq!(b.width, f.width, "{}: threads={threads} frame {i} width differs", path.display());
                    assert_eq!(b.height, f.height, "{}: threads={threads} frame {i} height differs", path.display());
                    assert_eq!(
                        b.yuv, f.yuv,
                        "{}: threads={threads} frame {i} is not byte-identical to threads=1",
                        path.display()
                    );
                }
            } else {
                baseline = Some(frames);
            }
        }
    }
}

/// `ffmpeg`'s own decode of the same file, as raw `yuv420p`, via the real
/// binary (Tier A black-box probing, D6/D7 — never `FFmpeg`'s source).
fn ffmpeg_reference_yuv(path: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg failed to decode {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// Black-box-oracle half of the acceptance criterion: this crate's decode
/// of each committed vector, compared byte-for-byte against `ffmpeg`'s own
/// decode of the same file. Requires the system `ffmpeg` binary, so this is
/// `#[ignore]`d rather than run on every `cargo test`.
#[test]
#[ignore = "shells out to the system ffmpeg binary; run explicitly with --ignored"]
fn decode_matches_ffmpeg_on_the_committed_vectors() {
    let mut vectors_checked = 0usize;
    for path in &fixture_paths() {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let ours = decode_all(&mut Vp9Decoder::new(Limits::default()), &bytes);
        assert!(!ours.is_empty(), "{}: decoded zero frames", path.display());

        let reference = ffmpeg_reference_yuv(path);
        let mut ref_offset = 0usize;
        let mut frames_compared = 0usize;
        for frame in &ours {
            let y_size = (frame.width as usize) * (frame.height as usize);
            let c_size = frame.width.div_ceil(2) as usize * frame.height.div_ceil(2) as usize;
            let frame_size = y_size + 2 * c_size;
            let Some(ref_frame) = reference.get(ref_offset..ref_offset + frame_size) else { break };
            ref_offset += frame_size;
            assert_eq!(&frame.yuv[..], ref_frame, "{}: frame {frames_compared} is not byte-identical to ffmpeg", path.display());
            frames_compared += 1;
        }
        assert!(
            frames_compared > 0,
            "{}: no comparable frames (ours={}, ffmpeg bytes={})",
            path.display(),
            ours.len(),
            reference.len()
        );
        println!("{:<40} frames={frames_compared} byte-exact vs ffmpeg", path.file_name().unwrap_or_default().to_string_lossy());
        vectors_checked += 1;
    }
    assert!(vectors_checked > 0, "no vectors were actually compared");
}
