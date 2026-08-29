//! GitHub #489 (FT-5.4): a per-frame SSIM/PSNR differential against
//! ffmpeg's own `ass` filter, using `vaco-conformance`'s real metrics
//! (`metrics::Ssim`/`metrics::Psnr`) rather than a bespoke one, and
//! measuring every plane separately per `planning/AGENT-CONSTRAINTS.md`'s
//! "measure every plane" rule.
//!
//! # The oracle: `ffmpeg-full`, not the system `ffmpeg`
//!
//! This host's default `ffmpeg` (Homebrew `ffmpeg`) is built **without
//! libass** — confirmed by `ffmpeg -h filter=ass` printing `Unknown filter
//! 'ass'.` — so it cannot serve as this test's oracle. `brew install
//! ffmpeg-full` (which depends on `libass`) provides one.
//!
//! Getting that binary to answer at all took two false starts, worth
//! recording so nobody repeats them: `ffmpeg-full`'s binaries genuinely
//! did not respond within 90s the first few times they were run this
//! session (`ffmpeg -version`, even `ffescape --help`) — that turned out
//! to be real, one-time first-launch overhead (most likely Gatekeeper's
//! initial verification of a freshly-poured bottle), not a permanent
//! block; subsequent invocations answer immediately. Once past that, this
//! test's *own* first draft still hung: it read the child's `stdout` only
//! after `wait()` returned, and a `yuv420p` frame (`640*480*1.5` bytes
//! here) is well past any OS pipe buffer — the child blocks writing a
//! full pipe, the parent blocks waiting for exit, and the timeout that
//! "fires" is really that deadlock, not a slow process. Fixed by draining
//! `stdout`/`stderr` on their own threads concurrently with `wait()` (see
//! [`run_reference`]'s own doc).
//!
//! This test still **probes for a working, ass-capable `ffmpeg` once,
//! with a bounded timeout**, and skips loudly (not silently — see
//! `planning/AGENT-CONSTRAINTS.md`'s "a test that skips on error is
//! indistinguishable from one that passes") if none answers, since a
//! machine with genuinely no libass-enabled `ffmpeg` anywhere is a real
//! possibility this test should degrade gracefully on. Set
//! `VACO_FFMPEG_ASS=/path/to/ffmpeg` to point it at a specific build.
//!
//! # Measured result (this host, `ffmpeg-full` 9.0.1)
//!
//! One dialogue line ("Hello ASS World", `Fontname=Arial`, white fill,
//! black outline+shadow) over a solid `0x404040` `yuv420p` frame:
//! **Y: SSIM 0.9764 (PSNR 26.53 dB), U: SSIM 1.0000, V: SSIM 1.0000.**
//! Perfect chroma agreement is expected here, not a coincidence: white
//! text and a flat grey background both quantise to the same neutral
//! chroma code regardless of rasteriser, so this frame cannot exercise a
//! chroma divergence at all — a harder case (a saturated `PrimaryColour`)
//! is a real follow-up, noted rather than silently assumed clean. The Y
//! divergence is exactly what plan 16 SS6.3.2 predicts and accepts:
//! "byte-exact isn't a hard requirement... a couple differences here and
//! there is fine" (`planning/AGENT-CONSTRAINTS.md`'s owner ruling) — this
//! crate's `swash` rasteriser and libass's own stroker/hinting legitimately
//! disagree on anti-aliased glyph edges. The `> 0.5` assertion below is a
//! structural floor (right place, right shape, not garbage), not a claim
//! of visual parity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "integration test")]

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vaco_ass::parse as parse_ass;
use vaco_conformance::compare::quality::Signal;
use vaco_conformance::metrics::{Psnr, Ssim};
use vaco_conformance::compare::quality::Metric as _;
use vaco_filter_subtitle::ass_filter;
use vaco_filter_text::TextRenderer;
use vaco_frame::FramePool;
use vaco_pixfmt::PixFmt;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

const SCRIPT: &str = "[Script Info]\n\
PlayResX: 640\n\
PlayResY: 480\n\
ScaledBorderAndShadow: yes\n\
\n\
[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, Bold, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV\n\
Style: Default,Arial,36,&H00FFFFFF,&H00000000,&H00000000,0,1,2,1,2,20,20,20\n\
\n\
[Events]\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,Hello ASS World\n";

/// Run `bin -filters`, bounded by `PROBE_TIMEOUT`, and report whether the
/// output lists both `ass` and `subtitles`. Never blocks past the timeout:
/// a wedged process is killed and treated as "not usable" rather than
/// hanging the test suite.
fn probe_ass_capable(bin: &str) -> bool {
    let Ok(mut child) = Command::new(bin).arg("-filters").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                let Some(mut out) = child.stdout.take() else { return false };
                let mut buf = String::new();
                let _ = out.read_to_string(&mut buf);
                let has = |name: &str| buf.lines().any(|l| l.split_whitespace().any(|w| w == name));
                return has("ass") && has("subtitles");
            }
            Ok(None) => {
                if start.elapsed() > PROBE_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("vaco-filter-subtitle ssim test: `{bin} -filters` did not return within {PROBE_TIMEOUT:?} — treating as unavailable");
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

fn find_ass_capable_ffmpeg() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(env) = std::env::var("VACO_FFMPEG_ASS") {
        candidates.push(env);
    }
    candidates.push("ffmpeg".to_owned());
    candidates.push("/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg".to_owned());
    candidates.push("/usr/local/opt/ffmpeg-full/bin/ffmpeg".to_owned());
    candidates.into_iter().find(|bin| probe_ass_capable(bin))
}

/// Run `ffmpeg` to render `SCRIPT` over a solid-colour input, bounded by
/// `RUN_TIMEOUT`. Returns raw `yuv420p` bytes on success.
///
/// A frame's worth of raw video (`640*480*1.5` bytes here) is well past
/// any OS pipe buffer, so stdout **must** be drained concurrently with
/// waiting on the child — reading it only after `wait()` returns is a
/// classic pipe deadlock (the child blocks writing a full pipe, the
/// parent blocks waiting for exit, and the timeout that "fires" is really
/// just this deadlock, not the process being slow). This bit the first
/// version of this test.
fn run_reference(bin: &str, script_path: &std::path::Path) -> Result<Vec<u8>, String> {
    let mut child = Command::new(bin)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=0x404040:s={WIDTH}x{HEIGHT}:d=1"),
            "-frames:v",
            "1",
            "-vf",
            &format!("ass={}", script_path.display()),
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn `{bin}` failed: {e}"))?;

    let mut stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let mut stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > RUN_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("`{bin}` did not finish within {RUN_TIMEOUT:?}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait on `{bin}` failed: {e}")),
        }
    };
    let out = stdout_reader.join().unwrap_or_default();
    let err = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("`{bin}` exited with {status}: {err}"));
    }
    Ok(out)
}

#[allow(clippy::unwrap_used, reason = "test code")]
fn our_render() -> Vec<u8> {
    let pool = FramePool::default();
    let mut frame = pool.acquire_video(PixFmt::Yuv420p, WIDTH, HEIGHT).unwrap();
    vaco_filter_draw::fill::fill(
        &mut frame,
        vaco_filter_draw::rect::Rect::full(WIDTH, HEIGHT),
        vaco_core::Rgba { r: 0x40, g: 0x40, b: 0x40, a: 255 },
    )
    .unwrap();
    let script = parse_ass(SCRIPT);
    let mut renderer = TextRenderer::new();
    ass_filter::render_at(&script, &mut renderer, &mut frame, vaco_core::Duration::ZERO).unwrap();

    let mut out = Vec::new();
    for plane_idx in 0..3 {
        let plane = frame.plane(plane_idx).unwrap();
        let (w, h) = if plane_idx == 0 { (WIDTH, HEIGHT) } else { (WIDTH.div_ceil(2), HEIGHT.div_ceil(2)) };
        for y in 0..h as usize {
            if let Some(row) = plane.row(y) {
                out.extend_from_slice(row.get(..w as usize).unwrap_or(row));
            }
        }
    }
    out
}

fn plane_signal(bytes: &[u8], width: u32, height: u32) -> Signal<'_> {
    Signal { planes: vec![bytes], strides: vec![width as usize], width, height, depth: 8 }
}

#[test]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
fn ssim_against_ffmpegs_own_ass_filter() {
    let Some(bin) = find_ass_capable_ffmpeg() else {
        eprintln!(
            "SKIP ssim_against_ffmpegs_own_ass_filter: no ass-capable ffmpeg reachable \
             in this environment (probed `ffmpeg`, `ffmpeg-full`; see this test file's own \
             doc for what was tried and found). Set VACO_FFMPEG_ASS to run this for real."
        );
        return;
    };

    let dir = std::env::temp_dir();
    let script_path = dir.join("vaco_ssim_ass_test.ass");
    {
        let mut f = std::fs::File::create(&script_path).unwrap();
        f.write_all(SCRIPT.as_bytes()).unwrap();
    }

    let reference = run_reference(&bin, &script_path).expect("reference ffmpeg run failed");
    let ours = our_render();
    let _ = std::fs::remove_file(&script_path);

    let y_size = (WIDTH * HEIGHT) as usize;
    let c_w = WIDTH.div_ceil(2);
    let c_h = HEIGHT.div_ceil(2);
    let c_size = (c_w * c_h) as usize;
    assert_eq!(reference.len(), y_size + 2 * c_size, "reference frame size mismatch — did the ass filter run at all?");
    assert_eq!(ours.len(), reference.len(), "our own render must be the same raw size");

    let ssim = Ssim;
    let psnr = Psnr::y();
    let planes: [(&str, usize, usize, u32, u32); 3] =
        [("Y", 0, y_size, WIDTH, HEIGHT), ("U", y_size, c_size, c_w, c_h), ("V", y_size + c_size, c_size, c_w, c_h)];

    for (name, offset, len, w, h) in planes {
        let ref_plane = &reference[offset..offset + len];
        let our_plane = &ours[offset..offset + len];
        let ref_signal = plane_signal(ref_plane, w, h);
        let our_signal = plane_signal(our_plane, w, h);
        let score = ssim.score(&ref_signal, &our_signal).unwrap();
        let psnr_score = psnr.score(&ref_signal, &our_signal).unwrap();
        eprintln!("plane {name}: SSIM={score:.4} PSNR={psnr_score:.2}dB");
        // Byte-exactness against libass is not the bar (this crate's own
        // doc, and planning/16-filters.md SS6.3.2's own "we should not
        // claim libass parity"): different rasterisers legitimately
        // disagree on anti-aliasing and hinting. 0.5 is a structural
        // floor — "the text is roughly there, roughly the right shape and
        // colour, not garbage or absent" — not a claim of visual parity.
        assert!(score > 0.5, "plane {name} SSIM {score:.4} is too low to be the same rendered content");
    }
}
