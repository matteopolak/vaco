//! Black-box validation of tetrahedral LUT evaluation.
//!
//! The table deliberately has non-affine corners and the six input pixels
//! occupy all six fractional-coordinate orders. A trilinear fallback can pass
//! identity and affine tests, but it cannot pass this comparison.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "a differential harness must report a failing reference command"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};

use vaco_scale::colour::Lut3D;

fn reference() -> Option<String> {
    let binary = std::env::var("VACO_REF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    Command::new(&binary)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success()
        .then_some(binary)
}

fn cube_text(values: &[[f64; 3]]) -> String {
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for value in values {
        writeln!(
            &mut text,
            "{:.17} {:.17} {:.17}",
            value[0], value[1], value[2]
        )
        .expect("format 3D LUT row");
    }
    text
}

#[test]
fn tetrahedral_lut_matches_the_reference_for_all_six_simplex_orders() {
    let Some(reference) = reference() else {
        eprintln!("SKIP: ffmpeg is not available");
        return;
    };
    // Red is fastest, matching the .cube format and Lut3D's constructor.
    let values = vec![
        [0.02, 0.03, 0.04],
        [0.94, 0.22, 0.11],
        [0.17, 0.88, 0.29],
        [0.72, 0.64, 0.95],
        [0.12, 0.31, 0.89],
        [0.81, 0.13, 0.67],
        [0.42, 0.93, 0.18],
        [0.98, 0.76, 0.54],
    ];
    let lut = Lut3D::from_values(2, values.clone()).expect("valid bounded lattice");
    // Each row has a different r/g/b ordering; no ordering branch is latent.
    let input = vec![
        223, 159, 64, 223, 64, 159, 159, 223, 64, 64, 223, 159, 159, 64, 223, 64, 159, 223,
    ];
    let path = std::env::temp_dir().join(format!("vaco-scale-118-{}.cube", std::process::id()));
    fs::write(&path, cube_text(&values)).expect("write temporary cube");
    let mut child = Command::new(&reference)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
        ])
        .args(["-s", "6x1", "-i", "pipe:0", "-vf"])
        .arg(format!("lut3d=file={}:interp=tetrahedral", path.display()))
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch ffmpeg");
    let mut stdin = child.stdin.take().expect("ffmpeg stdin");
    stdin.write_all(&input).expect("write fixture");
    drop(stdin);
    let output = child.wait_with_output().expect("wait for ffmpeg");
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "ffmpeg tetrahedral probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), input.len(), "reference output size");
    let mut max_error = 0u8;
    for (index, reference) in output.stdout.iter().enumerate() {
        let rgb = index / 3;
        let channel = index % 3;
        let sample = lut.sample([
            f64::from(input[rgb * 3]) / 255.0,
            f64::from(input[rgb * 3 + 1]) / 255.0,
            f64::from(input[rgb * 3 + 2]) / 255.0,
        ]);
        let ours = (sample[channel] * 255.0).floor().clamp(0.0, 255.0) as u8;
        max_error = max_error.max(ours.abs_diff(*reference));
        assert!(
            ours.abs_diff(*reference) <= 1,
            "pixel {rgb}, channel {channel}: ours {ours}, ffmpeg {reference}"
        );
    }
    println!("tetrahedral LUT max error against ffmpeg: {max_error} LSB");
}
