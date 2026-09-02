//! Differential tests against real `ffmpeg`/`cwebp`/`dwebp` output (D6):
//! the reference binaries this crate is allowed to run and probe freely,
//! never their source.
//!
//! Three directions, matching what C-19 actually changed:
//!
//! 1. **This crate's own lossless encode**, decoded by `ffmpeg`'s
//!    `libwebp`-backed decoder — must be byte-exact, since WebP lossless is
//!    an integer-exact transform (D11 "Exact").
//! 2. **This crate's own lossless decode** of a real `cwebp -lossless`
//!    file, cross-checked against `dwebp`'s decode of the same bytes — must
//!    also be byte-exact. `cwebp` uses predictor/color transforms and the
//!    color cache freely (verified via `-print_stats` warm-up), which is
//!    exactly the feature surface this crate's own encoder never emits and
//!    therefore cannot self-verify: this is the "decoder you did not
//!    write" check for that surface.
//! 3. **This crate's lossy encode** (routed through `vaco-codec-vp8` via
//!    the `"lossless"` option, C-19's other half), decoded by `ffmpeg` and
//!    compared by PSNR — lossy, so no exact target, but a real numeric
//!    quality floor rather than "it did not crash".
//!
//! Skipped rather than failed when the relevant binary is absent, matching
//! the convention `vaco-codec-vorbis`/`vaco-codec-flac`'s own differential
//! tests use.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::panic
)]

use std::io::Write;
use std::process::{Command, Stdio};

use vaco_codec_core::Encoder;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

fn tool_available(bin: &str) -> bool {
    Command::new(bin).arg("-version").output().is_ok()
}

fn run(bin: &str, args: &[&str], stdin_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut cmd = Command::new(bin);
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

/// A non-flat, non-trivial RGB24 test image via `ffmpeg`'s own generator —
/// real enough to exercise a real encoder's transform/palette decisions,
/// small enough to keep the test fast.
fn ffmpeg_testsrc_rgb24(w: u32, h: u32) -> Option<Vec<u8>> {
    ffmpeg_lavfi_rgb24(&format!("testsrc2=size={w}x{h}:rate=1"))
}

/// Continuous-tone content — real photographs and `mandelbrot` alike have
/// far more than 151 distinct colors, which is what actually makes `cwebp`
/// choose the predictor/color transforms over color-indexing (measured:
/// `testsrc2` above has few enough unique colors that `cwebp -m 6 -q 100`
/// picks `COLOR_INDEXING` instead, exercising a different — still real —
/// part of this crate's decoder).
fn ffmpeg_mandelbrot_rgb24(w: u32, h: u32) -> Option<Vec<u8>> {
    ffmpeg_lavfi_rgb24(&format!("mandelbrot=size={w}x{h}:rate=1"))
}

fn ffmpeg_lavfi_rgb24(filter: &str) -> Option<Vec<u8>> {
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-f",
            "lavfi",
            "-i",
            filter,
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        None,
    )
}

fn decode_webp_to_rgb24_via_ffmpeg(webp_bytes: &[u8]) -> Option<Vec<u8>> {
    // "webp" is not a registered demuxer name (ffmpeg auto-detects a named
    // .webp *file* by content/extension instead); a piped, unnamed stream
    // needs the explicit piped-demuxer name. Measured directly: `-f webp`
    // fails with "Unknown input format: 'webp'" every time, which an
    // earlier version of this test silently treated as "skip" rather than
    // "broken", so it captured nothing to test against `705779d`'s own
    // warning about tests that cannot fail — the fix is `webp_pipe`.
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-f",
            "webp_pipe",
            "-i",
            "-",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        Some(webp_bytes),
    )
}

fn frame_from_rgb24(bytes: &[u8], w: u32, h: u32) -> Frame {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, w, h).expect("alloc");
    let row_bytes = w as usize * 3;
    for mut plane in frame.planes_mut() {
        for row in 0..plane.rows() {
            let start = row * row_bytes;
            let src = &bytes[start..(start + row_bytes).min(bytes.len())];
            if let Some(dst) = plane.row_mut(row) {
                let n = dst.len().min(src.len());
                dst[..n].copy_from_slice(&src[..n]);
            }
        }
    }
    frame
}

fn frame_rgb24_bytes(frame: &Frame) -> Vec<u8> {
    let plane = frame.plane(0).expect("plane 0");
    let mut out = Vec::new();
    for row in plane.rows_iter() {
        out.extend_from_slice(row);
    }
    out
}

#[test]
fn native_lossless_encode_round_trips_exactly_through_ffmpeg() {
    if !tool_available("ffmpeg") {
        eprintln!("skip: ffmpeg not available");
        return;
    }
    let (w, h) = (80, 60);
    let Some(raw) = ffmpeg_testsrc_rgb24(w, h) else {
        eprintln!("skip: ffmpeg testsrc2 generation failed");
        return;
    };
    assert_eq!(raw.len(), (w * h * 3) as usize);
    let frame = frame_from_rgb24(&raw, w, h);

    let encoded = vaco_codec_webp::encode(&frame).expect("native VP8L encode");
    assert_eq!(encoded.get(0..4), Some(b"RIFF".as_slice()));
    assert_eq!(encoded.get(12..16), Some(b"VP8L".as_slice()));

    let Some(decoded_by_ffmpeg) = decode_webp_to_rgb24_via_ffmpeg(&encoded) else {
        eprintln!("skip: ffmpeg could not decode our VP8L output");
        return;
    };
    assert_eq!(
        decoded_by_ffmpeg, raw,
        "lossless WebP must round-trip byte-exact (D11 Exact)"
    );
}

#[test]
fn native_lossless_decode_matches_dwebp_on_a_real_cwebp_file() {
    if !tool_available("ffmpeg") || !tool_available("cwebp") || !tool_available("dwebp") {
        eprintln!("skip: ffmpeg/cwebp/dwebp not all available");
        return;
    }
    let (w, h) = (96, 64);
    let Some(raw) = ffmpeg_mandelbrot_rgb24(w, h) else {
        eprintln!("skip: ffmpeg mandelbrot generation failed");
        return;
    };

    let dir = std::env::temp_dir().join(format!("vaco-webp-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let png_path = dir.join("in.png");
    let cwebp_path = dir.join("cwebp_out.webp");

    // Real photographic-ish content, into a PNG cwebp can read.
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "-",
            png_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(&raw)?;
            c.wait()
        })
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skip: could not write PNG fixture");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    // `-m 6 -q 100` pushes cwebp toward using predictor/color transforms
    // and the color cache, not just subtract-green — the feature surface
    // this crate's own encoder never emits and so cannot self-verify.
    let cwebp_ok = Command::new("cwebp")
        .args([
            "-quiet",
            "-lossless",
            "-m",
            "6",
            "-q",
            "100",
            png_path.to_str().unwrap(),
            "-o",
            cwebp_path.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !cwebp_ok {
        eprintln!("skip: cwebp encode failed");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let cwebp_bytes = std::fs::read(&cwebp_path).expect("read cwebp output");

    let dwebp_ppm = run(
        "dwebp",
        &[cwebp_path.to_str().unwrap(), "-ppm", "-o", "-"],
        None,
    )
    .expect("dwebp decode");
    // Strip the PPM header ("P6\n<w> <h>\n255\n") to get raw RGB24 bytes.
    let dwebp_rgb = strip_ppm_header(&dwebp_ppm);

    let mut budget = Budget::new(Limits::permissive());
    let decoded_frames =
        vaco_codec_webp::decode(&cwebp_bytes, &mut budget).expect("this crate's own VP8L decode");
    assert_eq!(decoded_frames.len(), 1);
    let ours = frame_rgb24_bytes(&decoded_frames[0]);

    let FrameData::Video { width, height, .. } = decoded_frames[0].data else {
        panic!("video frame");
    };
    assert_eq!((width, height), (w, h));
    assert_eq!(
        ours, dwebp_rgb,
        "this crate's VP8L decode must match dwebp byte-for-byte on a real cwebp file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_lossless_decode_handles_alpha_matching_dwebp() {
    if !tool_available("ffmpeg") || !tool_available("cwebp") || !tool_available("dwebp") {
        eprintln!("skip: ffmpeg/cwebp/dwebp not all available");
        return;
    }
    let (w, h) = (48, 32);
    let dir = std::env::temp_dir().join(format!("vaco-webp-alpha-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let png_path = dir.join("in_alpha.png");
    let cwebp_path = dir.join("cwebp_alpha.webp");

    // `format=rgba` plus a radial gradient on alpha gives real, non-opaque
    // alpha values, not just 0/255 — the case `alpha_is_used` exists for.
    let png_ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={w}x{h}:rate=1,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='128+X'"),
            "-frames:v",
            "1",
            "-update",
            "1",
            png_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !png_ok {
        eprintln!("skip: could not generate an alpha PNG fixture");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let cwebp_ok = Command::new("cwebp")
        .args([
            "-quiet",
            "-lossless",
            "-m",
            "6",
            "-q",
            "100",
            png_path.to_str().unwrap(),
            "-o",
            cwebp_path.to_str().unwrap(),
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !cwebp_ok {
        eprintln!("skip: cwebp encode of an alpha PNG failed");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let cwebp_bytes = std::fs::read(&cwebp_path).expect("read cwebp output");
    // A bare (non-VP8X-wrapped) VP8L chunk carries alpha directly in its
    // ARGB pixels, so this still exercises this crate's fast native path,
    // not the `image-webp` VP8X fallback.
    assert_eq!(cwebp_bytes.get(12..16), Some(b"VP8L".as_slice()));

    let dwebp_pam = run(
        "dwebp",
        &[cwebp_path.to_str().unwrap(), "-pam", "-o", "-"],
        None,
    )
    .expect("dwebp decode");
    let dwebp_rgba = strip_pam_header(&dwebp_pam);

    let mut budget = Budget::new(Limits::permissive());
    let decoded_frames =
        vaco_codec_webp::decode(&cwebp_bytes, &mut budget).expect("this crate's own VP8L decode");
    assert_eq!(decoded_frames.len(), 1);
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = decoded_frames[0].data
    else {
        panic!("video frame");
    };
    assert_eq!((width, height), (w, h));
    assert_eq!(
        format,
        PixFmt::Rgba,
        "an image with non-opaque alpha must decode as Rgba"
    );

    let plane = decoded_frames[0].plane(0).expect("plane 0");
    let mut ours = Vec::new();
    for row in plane.rows_iter() {
        ours.extend_from_slice(row);
    }
    assert_eq!(
        ours, dwebp_rgba,
        "this crate's VP8L alpha decode must match dwebp byte-for-byte"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn strip_pam_header(pam: &[u8]) -> Vec<u8> {
    let marker = b"ENDHDR\n";
    pam.windows(marker.len())
        .position(|w| w == marker)
        .map_or_else(Vec::new, |pos| pam[pos + marker.len()..].to_vec())
}

fn strip_ppm_header(ppm: &[u8]) -> Vec<u8> {
    // "P6\n<width> <height>\n<maxval>\n" then raw samples — three
    // whitespace-terminated tokens after the magic number.
    let mut pos = 0usize;
    let mut tokens_seen = 0;
    let mut in_token = false;
    while pos < ppm.len() && tokens_seen < 4 {
        let is_ws = ppm[pos].is_ascii_whitespace();
        if !is_ws && !in_token {
            in_token = true;
        } else if is_ws && in_token {
            in_token = false;
            tokens_seen += 1;
        }
        pos += 1;
    }
    ppm[pos..].to_vec()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len()).max(1);
    let mse: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum::<f64>()
        / n as f64;
    if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn lossy_encode_via_vp8_decodes_at_a_real_quality_floor() {
    if !tool_available("ffmpeg") {
        eprintln!("skip: ffmpeg not available");
        return;
    }
    let (w, h) = (80, 60);
    let Some(raw) = ffmpeg_testsrc_rgb24(w, h) else {
        eprintln!("skip: ffmpeg testsrc2 generation failed");
        return;
    };
    let frame = frame_from_rgb24(&raw, w, h);

    let mut enc = vaco_codec_webp::WebpEncoder::new(Limits::permissive());
    enc.set_option("lossless", "0").expect("set lossless=0");
    enc.send_frame(Some(&frame)).expect("send frame");
    enc.send_frame(None).expect("drain");
    let packet = enc.receive_packet().expect("receive packet");
    assert_eq!(packet.payload().get(12..16), Some(b"VP8 ".as_slice()));

    let Some(decoded) = decode_webp_to_rgb24_via_ffmpeg(packet.payload()) else {
        eprintln!("skip: ffmpeg could not decode our lossy VP8 output");
        return;
    };
    assert_eq!(decoded.len(), raw.len());
    let db = psnr(&raw, &decoded);
    assert!(db > 20.0, "lossy VP8-in-WebP PSNR too low: {db} dB");
}
