//! Pins the one place this crate knowingly differs from the reference.
//!
//! D17 says to reproduce an observable deviation; D17.1 carves out the cases
//! where reproducing it would mean committing to something we should not. This
//! is one of those, and the rule D17.1 attaches is what this file implements:
//! **diverge minimally, and pin the divergence in a test that asserts it still
//! exists**, so that the reference changing — or us finding a way to close the
//! gap — is a test failure rather than a silent drift.
//!
//! Absence of `ffmpeg` is a skip.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    clippy::wildcard_imports,
    clippy::print_stdout,
    clippy::too_many_arguments,
    reason = "a conformance probe whose output is the deliverable"
)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

fn reference() -> Option<String> {
    let bin = std::env::var("VACO_REF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    Command::new(&bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success()
        .then_some(bin)
}

/// One `yuv444p` pixel through the reference, as `rgb24`.
fn reference_pixel(bin: &str, y: u8, u: u8, v: u8) -> Option<[u8; 3]> {
    let mut child = Command::new(bin)
        .args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo"])
        .args(["-pix_fmt", "yuv444p", "-s", "1x1", "-i", "pipe:0"])
        .args([
            "-vf",
            "scale=in_range=tv:out_range=pc:in_color_matrix=bt709",
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let writer = std::thread::spawn(move || stdin.write_all(&[y, u, v]));
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !out.status.success() || out.stdout.len() < 3 {
        return None;
    }
    Some([out.stdout[0], out.stdout[1], out.stdout[2]])
}

fn our_pixel(y: u8, u: u8, v: u8) -> [u8; 3] {
    let color = ColorInfo {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt709,
        ..ColorInfo::default()
    };
    let src = ImageSpec::new(PixFmt::Yuv444p, 1, 1).with_color(color);
    let dst = ImageSpec::new(PixFmt::Rgb24, 1, 1).with_color(ColorInfo {
        range: ColorRange::Full,
        matrix: MatrixCoefficients::Bt709,
        ..ColorInfo::default()
    });
    let mut scaler = Scaler::new(&src, &dst, &ScaleOptions::default()).expect("plans");
    let (py, pu, pv) = ([y], [u], [v]);
    let srcs = [
        SrcPlane {
            data: &py,
            stride: 1,
        },
        SrcPlane {
            data: &pu,
            stride: 1,
        },
        SrcPlane {
            data: &pv,
            stride: 1,
        },
    ];
    let mut out = [0u8; 3];
    {
        let mut dsts = [DstPlane {
            data: &mut out,
            stride: 3,
        }];
        scaler.scale_planes(&srcs, &mut dsts).expect("converts");
    }
    out
}

/// The reference emits 0 where the pre-clip blue value reaches 512; we saturate.
///
/// `Y = 225, U = 255, V = 128` at BT.709 limited range gives a pre-clip blue of
/// `(9539·209 + 17305·127 + 4096) >> 13 = 512`, which is the first value past the
/// end of its clipping table.
#[test]
fn the_reference_still_wraps_where_we_saturate() {
    let Some(bin) = reference() else {
        println!("SKIP: no reference ffmpeg");
        return;
    };
    let (y, u, v) = (225u8, 255u8, 128u8);
    let ours = our_pixel(y, u, v);
    let Some(theirs) = reference_pixel(&bin, y, u, v) else {
        panic!("reference run failed");
    };

    assert_eq!(ours[2], 255, "we must saturate");
    assert_eq!(
        theirs[2], 0,
        "the reference defect this crate deliberately does not reproduce has \
         changed: it emitted {} rather than 0 for the blue channel. Re-read \
         vaco_scale::REFERENCE_CLIP_DIVERGENCE and docs/signal/vaco-scale.md \
         section 5.6 before touching anything.",
        theirs[2]
    );

    // One value below the threshold, both must agree — the divergence is a
    // cliff, not a drift.
    let (y, u) = (224u8, 255u8);
    let ours = our_pixel(y, u, v);
    let theirs = reference_pixel(&bin, y, u, v).expect("reference run");
    assert_eq!(
        ours, theirs,
        "below the threshold the two implementations must agree exactly"
    );
}
