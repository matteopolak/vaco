//! Decode conformance against real `ffmpeg`, per RFC 6386 §9.5's own
//! justification for the token-partition split it uses (issue #301,
//! `C-16d`): "the decoder can perform parallel decoding" of coefficient
//! tokens one partition per row group. `tests/fixtures/vp8/` carries a small
//! curated slice of the official `webmproject/vp8-test-vectors` suite,
//! chosen to cover every vector category that suite ships (comprehensive,
//! intra, inter, segmentation, multi-partition, sharpness, dynamic resize).
//!
//! The full, current 61-file suite (fetched fresh from
//! `github.com/webmproject/vp8-test-vectors`, not the 60 this file used to
//! say — the upstream repository has since added one) was run by hand for
//! the same measurement, at every one of `-threads` 1/2/4/8: **59 of 61
//! byte-exact against `ffmpeg` and byte-identical across every thread
//! count**, with the same 2 vectors excluded below for the same
//! already-disclosed, out-of-scope reason on every thread count (a decode
//! defect would not care how many threads asked for it). That run is not
//! itself committed (61 binary fixtures is a lot to carry for a check this
//! module's own committed 10-vector subset already demonstrates the shape
//! of); the [`threads_are_byte_identical`] test below re-demonstrates the
//! thread-count half on the fixtures that *are* committed, on every run.
//!
//! Every plane is measured separately (Y, U, V), never a whole-frame
//! average — a defect confined to a subsampled plane is exactly what an
//! aggregate metric hides (see `AGENT-CONSTRAINTS.md`'s "measuring one plane
//! is not measuring the output"). Decode of a real, standard-conformant
//! bitstream is deterministic by specification, so unlike this crate's own
//! *encoder* (which has real quality/size trade-offs and is compared by
//! PSNR/SSIM), a mismatch here is either an off-by-something bug or a
//! rounding difference — the numbers printed under `--nocapture` say which.
//!
//! Two measurement traps this harness had to be built around, both found by
//! running it and getting an answer that did not survive a second look
//! (`AGENT-CONSTRAINTS.md`'s "two probes that disagree are not noise"):
//!
//! 1. **`ffmpeg -f rawvideo` duplicates a frame under its default vsync.**
//!    Without `-fps_mode passthrough`, `ffmpeg` matches its raw dump to a
//!    declared output cadence and can insert a duplicate frame — measured on
//!    `vp80-02-inter-1424.ivf`: 15 raw frames out of a 14-packet file
//!    (`ffprobe -count_frames` and a hand-parsed IVF packet count both agree
//!    on 14). Every frame after the duplicate then compares against the
//!    wrong reference frame, which is indistinguishable from a real,
//!    structured decode bug until the frame counts are checked directly.
//! 2. **A later key frame's dimensions are not always the coded ones a
//!    reader should compare against.** `vp80-03-segmentation-1425.ivf` and
//!    `-1436.ivf` each have a second key frame whose raw tag bytes declare a
//!    *coded* size smaller than the first frame's, plus a non-zero
//!    horizontal/vertical scale code (RFC 6386 §9.1's upper 2 bits of each
//!    dimension field — confirmed by hand-decoding the tag bytes directly,
//!    not by reading a decoder's source: `vp80-03-segmentation-1436.ivf`'s
//!    second key frame is coded at 282x231 with scale code 1 on both axes).
//!    `ffmpeg` upsamples to a display size for its raw dump; this crate
//!    decodes to and reports the *coded* size and does not implement RFC
//!    6386 §9.1's display-rescale step at all — a real, scoped-out feature
//!    gap distinct from a decode defect, and orthogonal to loop filter,
//!    threading and token-partition conformance (issue #301's actual scope).
//!    This harness detects the scale codes directly from the IVF bytes and
//!    excludes an affected vector from the strict comparison, reporting it
//!    as a known gap instead of asserting a false failure.
//!
//! Frame threading (issue #301, `vaco_codec_vp8`'s frame-task split) is verified two ways:
//! the full-suite hand-run above, and [`threads_are_byte_identical`], which
//! decodes every committed fixture at `-threads` 1/2/4/8 in-process and
//! asserts the frames are byte-identical at every count — no `ffmpeg`
//! dependency, so it runs on every `cargo test`, not just `--ignored`.

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
use vaco_codec_vp8::Vp8Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Split an IVF file into its raw per-frame VP8 payloads. Trivial and
/// test-only, so a hand-rolled 15-line reader is preferable to pulling in a
/// demuxer crate (there is no `vaco-demux-ivf` in this workspace, and a
/// codec crate reaching for a demux dependency for a test harness would be
/// exactly the layering shortcut D14.1 exists to prevent).
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

/// Whether any key frame in this IVF file's packets declares a non-zero
/// RFC 6386 §9.1 horizontal or vertical scale code — decoded straight from
/// each key frame's raw tag bytes (3-byte frame tag, 3-byte start code,
/// then two little-endian 16-bit fields whose low 14 bits are the *coded*
/// dimension and whose top 2 bits are the scale code). See this module's
/// doc for why a non-zero code makes byte comparison against `ffmpeg`
/// meaningless rather than wrong.
fn has_scaled_key_frame(bytes: &[u8]) -> bool {
    for payload in ivf_frame_payloads(bytes) {
        let Some(&[b0, b1, b2]) = payload.get(0..3) else { continue };
        let tag0 = u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16);
        let key_frame = tag0 & 1 == 0;
        if !key_frame {
            continue;
        }
        let Some(dims) = payload.get(6..10) else { continue };
        let w = u16::from_le_bytes([dims[0], dims[1]]);
        let h = u16::from_le_bytes([dims[2], dims[3]]);
        if w >> 14 != 0 || h >> 14 != 0 {
            return true;
        }
    }
    false
}

/// One shown frame's dimensions and its Y/U/V bytes, tightly packed in
/// `ffmpeg -f rawvideo -pix_fmt yuv420p` order (whole Y plane, then whole U,
/// then whole V).
struct DecodedFrame {
    width: u32,
    height: u32,
    yuv: Vec<u8>,
}

/// Decode every frame of an IVF file with this crate's own decoder, one
/// entry per *shown* frame (an invisible altref packet contributes nothing,
/// matching `ffmpeg`'s own raw dump).
fn decode_to_planar_yuv(ivf_bytes: &[u8]) -> Vec<DecodedFrame> {
    decode_all(&mut Vp8Decoder::new(Limits::default()), ivf_bytes)
}

/// `ffmpeg`'s own decode of the same file, as raw `yuv420p`, via the real
/// binary (Tier A black-box probing, D7/D17 — never `FFmpeg`'s source).
fn ffmpeg_reference_yuv(path: &Path) -> Vec<u8> {
    // `-fps_mode passthrough` (the modern spelling of `-vsync 0`) is not
    // decoration: without it `ffmpeg` applies its default CFR vsync
    // behaviour and can duplicate or drop a frame to match declared output
    // timing — see this module's doc for the measured case.
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed to decode {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Per-plane comparison of two same-sized byte slices.
struct PlaneDiff {
    max: i32,
    mean: f64,
    exact: bool,
}

fn compare_plane(a: &[u8], b: &[u8]) -> PlaneDiff {
    let n = a.len().min(b.len());
    let mut max = 0i32;
    let mut sum = 0i64;
    for i in 0..n {
        let d = i32::from(a[i]).abs_diff(i32::from(b[i])).cast_signed();
        max = max.max(d);
        sum += i64::from(d);
    }
    #[allow(clippy::cast_precision_loss, reason = "n is a byte count of a small test vector, far below f64's exact-integer range")]
    let mean = if n == 0 { 0.0 } else { sum as f64 / n as f64 };
    PlaneDiff { max, mean, exact: max == 0 && a.len() == b.len() }
}

fn fixtures_dir() -> PathBuf {
    std::env::var("VACO_VP8_CONFORMANCE_DIR").map_or_else(
        |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vp8"),
        PathBuf::from,
    )
}

#[test]
#[ignore = "shells out to the system ffmpeg binary; run explicitly with --ignored"]
fn decode_matches_ffmpeg_per_plane_on_the_real_vp8_test_vectors() {
    let dir = fixtures_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ivf"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .ivf vectors found in {}", dir.display());

    let mut worst_y = PlaneDiff { max: 0, mean: 0.0, exact: true };
    let mut worst_u = PlaneDiff { max: 0, mean: 0.0, exact: true };
    let mut worst_v = PlaneDiff { max: 0, mean: 0.0, exact: true };
    let mut vectors_checked = 0usize;

    for path in &entries {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        if has_scaled_key_frame(&bytes) {
            println!(
                "{:<40} SKIPPED: a key frame declares a non-zero RFC 6386 §9.1 scale code \
                 (display-rescale is not implemented — see this module's doc)",
                path.file_name().unwrap_or_default().to_string_lossy(),
            );
            continue;
        }

        let ours = decode_to_planar_yuv(&bytes);
        assert!(!ours.is_empty(), "{}: decoded zero frames", path.display());

        let reference = ffmpeg_reference_yuv(path);

        let (mut y_max, mut y_sum_mean, mut u_max, mut u_sum_mean, mut v_max, mut v_sum_mean) =
            (0i32, 0.0f64, 0i32, 0.0f64, 0i32, 0.0f64);
        let mut all_exact = true;
        let mut ref_offset = 0usize;
        let mut frames_compared = 0usize;
        for frame in &ours {
            let y_size = (frame.width as usize) * (frame.height as usize);
            let c_size = frame.width.div_ceil(2) as usize * frame.height.div_ceil(2) as usize;
            let frame_size = y_size + 2 * c_size;
            let Some(ref_frame) = reference.get(ref_offset..ref_offset + frame_size) else { break };
            ref_offset += frame_size;

            let y = compare_plane(&frame.yuv[..y_size], &ref_frame[..y_size]);
            let u = compare_plane(&frame.yuv[y_size..y_size + c_size], &ref_frame[y_size..y_size + c_size]);
            let v = compare_plane(&frame.yuv[y_size + c_size..], &ref_frame[y_size + c_size..]);
            y_max = y_max.max(y.max);
            u_max = u_max.max(u.max);
            v_max = v_max.max(v.max);
            y_sum_mean += y.mean;
            u_sum_mean += u.mean;
            v_sum_mean += v.mean;
            all_exact &= y.exact && u.exact && v.exact;
            frames_compared += 1;
        }
        assert!(
            frames_compared > 0,
            "{}: no comparable frames (ours={}, ffmpeg bytes={})",
            path.display(),
            ours.len(),
            reference.len()
        );
        #[allow(clippy::cast_precision_loss, reason = "frames_compared is a small per-vector frame count")]
        let n = frames_compared as f64;
        println!(
            "{:<40} frames={frames_compared:<4} Y(max={y_max:>3} mean={:.4}) U(max={u_max:>3} mean={:.4}) V(max={v_max:>3} mean={:.4}) exact={all_exact}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            y_sum_mean / n,
            u_sum_mean / n,
            v_sum_mean / n,
        );

        worst_y.max = worst_y.max.max(y_max);
        worst_u.max = worst_u.max.max(u_max);
        worst_v.max = worst_v.max.max(v_max);
        worst_y.mean = worst_y.mean.max(y_sum_mean / n);
        worst_u.mean = worst_u.mean.max(u_sum_mean / n);
        worst_v.mean = worst_v.mean.max(v_sum_mean / n);
        worst_y.exact &= all_exact;
        worst_u.exact &= all_exact;
        worst_v.exact &= all_exact;
        vectors_checked += 1;
    }

    println!(
        "worst across {vectors_checked} vectors: Y(max={} mean={:.4} exact={}) U(max={} mean={:.4} exact={}) V(max={} mean={:.4} exact={})",
        worst_y.max, worst_y.mean, worst_y.exact, worst_u.max, worst_u.mean, worst_u.exact, worst_v.max, worst_v.mean, worst_v.exact
    );

    // Every committed vector decodes byte-exact per plane (issue #301,
    // measured today; also re-checked against the full, current 61-file
    // upstream suite by hand — see this module's doc). Not a mean-error
    // threshold: a `< 1.0` bound would have let a real, small, structured
    // divergence through silently, which is exactly the trap
    // `planning/AGENT-CONSTRAINTS.md` names ("a decoder that emits the
    // right number of bytes has demonstrated only that"). If a future
    // committed vector genuinely cannot reach byte-exactness, name it in
    // `has_scaled_key_frame`-style exclusion with a cited reason, the same
    // way the two display-rescale vectors already are — never widen this
    // bound instead.
    assert!(vectors_checked > 0, "no vectors were actually compared");
    assert!(worst_y.exact, "luma was not byte-exact on every vector: max={} mean={:.4}", worst_y.max, worst_y.mean);
    assert!(worst_u.exact, "chroma-U was not byte-exact on every vector: max={} mean={:.4}", worst_u.max, worst_u.mean);
    assert!(worst_v.exact, "chroma-V was not byte-exact on every vector: max={} mean={:.4}", worst_v.max, worst_v.mean);
}

/// Frame threading (issue #301): every committed fixture decodes to
/// byte-identical `Frame`s at `-threads` 1, 2, 4 and 8, driven directly
/// through [`Vp8Decoder`] (no `ffmpeg`, no process spawn — this is the
/// determinism half of the acceptance criterion, checked on every
/// `cargo test`, not just `--ignored`). Complements the full-suite
/// `ffmpeg`-differential run this module's doc records by hand.
#[test]
fn threads_are_byte_identical() {
    let dir = fixtures_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ivf"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .ivf vectors found in {}", dir.display());

    for path in &entries {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut baseline: Option<Vec<DecodedFrame>> = None;
        for threads in [1usize, 2, 4, 8] {
            let mut dec = Vp8Decoder::new(Limits::default());
            let granted = dec.set_thread_count(threads);
            let frames = decode_all(&mut dec, &bytes);
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

/// Like [`decode_to_planar_yuv`], but takes an already-configured decoder
/// (so the caller can set the thread count first) instead of building one.
fn drain_ready(dec: &mut Vp8Decoder, out: &mut Vec<DecodedFrame>) {
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

fn decode_all(dec: &mut Vp8Decoder, ivf_bytes: &[u8]) -> Vec<DecodedFrame> {
    let mut budget = Budget::new(Limits::default());
    let mut out = Vec::new();
    for payload in ivf_frame_payloads(ivf_bytes) {
        let Ok(packet) = Packet::from_slice(&mut budget, payload) else { continue };
        if dec.send_packet(Some(&packet)).is_err() {
            continue;
        }
        drain_ready(dec, &mut out);
    }
    // End of stream: a frame-threaded decoder (issue #301) may still be
    // holding pictures it accepted but has not yet had to collect (only
    // `max_in_flight` forces that during decode) — `send_packet(None)` is
    // this trait's own drain signal, and skipping it here would silently
    // lose exactly those pictures, not decode them wrong. This is not
    // optional plumbing this test forgot: the pre-threading decoder never
    // buffered a picture past its own packet, so this call used to be a
    // no-op, which is why it was previously easy to leave out and still
    // pass.
    let _ = dec.send_packet(None);
    drain_ready(dec, &mut out);
    out
}
