//! Full CLI paths checked with an independent encoder/decoder binary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration tests fail loudly on invalid fixture assumptions"
)]

use std::process::Command;

const YUV420_FRAME_BYTES: usize = 64 * 64 + 2 * 32 * 32;

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn run(args: &[&str]) -> (i32, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = vaco_cli::run(args, &mut out, &mut err);
    (status.code(), String::from_utf8_lossy(&err).into_owned())
}

fn succeeds(args: &[&str]) {
    let (code, error) = run(args);
    assert_eq!(code, 0, "vaco {args:?}: {error}");
}

#[test]
fn limits_keep_exact_video_bytes_across_copy_encode_and_zero() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.y4m");
    let source = source.to_str().unwrap();
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=10:duration=2",
        "-f",
        "yuv4mpegpipe",
        source,
    ]);
    for codec in ["copy", "rawvideo"] {
        for (limit, count) in [("0", 0), ("1", 1), ("7", 7), ("30", 20)] {
            let output = dir.path().join(format!("{codec}-{limit}.yuv"));
            succeeds(&[
                "-y",
                "-i",
                source,
                "-c:v",
                codec,
                "-frames:v",
                limit,
                "-f",
                "rawvideo",
                output.to_str().unwrap(),
            ]);
            let actual = std::fs::read(output).unwrap();
            let reference = ffmpeg(&["-i", source, "-frames:v", limit, "-f", "rawvideo", "-"]);
            assert_eq!(actual.len(), count * YUV420_FRAME_BYTES);
            assert_eq!(actual, reference, "codec={codec}, limit={limit}");
        }
    }
    let a = dir.path().join("first.yuv");
    let b = dir.path().join("second.yuv");
    succeeds(&[
        "-y",
        "-i",
        source,
        "-c:v",
        "copy",
        "-vframes",
        "3",
        "-f",
        "rawvideo",
        a.to_str().unwrap(),
        "-c:v",
        "rawvideo",
        "-frames:v",
        "9",
        "-f",
        "rawvideo",
        b.to_str().unwrap(),
    ]);
    assert_eq!(
        std::fs::metadata(a).unwrap().len(),
        (3 * YUV420_FRAME_BYTES) as u64
    );
    assert_eq!(
        std::fs::metadata(b).unwrap().len(),
        (9 * YUV420_FRAME_BYTES) as u64
    );
    let graph_output = dir.path().join("graph.yuv");
    succeeds(&[
        "-y",
        "-i",
        source,
        "-filter_complex",
        "[0:v]hflip[v]",
        "-map",
        "[v]",
        "-c:v",
        "rawvideo",
        "-frames:v",
        "5",
        "-f",
        "rawvideo",
        graph_output.to_str().unwrap(),
    ]);
    let graph_reference = ffmpeg(&[
        "-i",
        source,
        "-vf",
        "hflip",
        "-frames:v",
        "5",
        "-f",
        "rawvideo",
        "-",
    ]);
    let graph_actual = std::fs::read(graph_output).unwrap();
    assert_eq!(graph_actual.len(), 5 * YUV420_FRAME_BYTES);
    assert_eq!(graph_actual, graph_reference);
}

#[test]
fn cli_two_pass_statistics_are_written_consumed_and_decodable() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.y4m");
    let source = source.to_str().unwrap();
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=10:duration=2",
        "-f",
        "yuv4mpegpipe",
        source,
    ]);
    for (tool, encoder, format) in [("x264", "libx264", "h264"), ("x265", "libx265", "hevc")] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("skipping {encoder}: {tool} is not installed");
            continue;
        }
        let prefix = dir.path().join(tool);
        let prefix = prefix.to_str().unwrap();
        let first = dir.path().join(format!("first.{format}"));
        let second = dir.path().join(format!("second.{format}"));
        for (pass, output) in [("1", &first), ("2", &second)] {
            succeeds(&[
                "-y",
                "-i",
                source,
                "-c:v",
                encoder,
                "-b:v",
                "100k",
                "-frames:v",
                "12",
                "-pass",
                pass,
                "-passlogfile",
                prefix,
                "-f",
                format,
                output.to_str().unwrap(),
            ]);
            let decoded = ffmpeg(&[
                "-i",
                output.to_str().unwrap(),
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "-",
            ]);
            assert_eq!(
                decoded.len(),
                12 * YUV420_FRAME_BYTES,
                "{encoder} pass {pass}"
            );
        }
        let logfile = format!("{prefix}-0.log");
        assert!(std::fs::metadata(&logfile).unwrap().len() > 100);
        std::fs::write(&logfile, b"invalid statistics").unwrap();
        let (code, error) = run(&[
            "-y",
            "-i",
            source,
            "-c:v",
            encoder,
            "-b:v",
            "100k",
            "-pass",
            "2",
            "-passlogfile",
            prefix,
            "-f",
            format,
            second.to_str().unwrap(),
        ]);
        assert_ne!(code, 0, "corrupt stats were silently ignored: {error}");
    }
    let (code, error) = run(&[
        "-i", source, "-c:v", "rawvideo", "-pass", "1", "-f", "null", "-",
    ]);
    assert_ne!(code, 0);
    assert!(error.contains("two-pass"), "{error}");
}

#[test]
fn audio_limits_preserve_sample_count_and_channel_bytes() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("stereo.wav");
    let source = source.to_str().unwrap();
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=sin(2*PI*997*t)|0.5*sin(2*PI*431*t):s=48000:d=0.3",
        "-c:a",
        "pcm_s16le",
        source,
    ]);
    for codec in ["copy", "pcm_s16le"] {
        let output = dir.path().join(format!("{codec}.pcm"));
        succeeds(&[
            "-y",
            "-i",
            source,
            "-c:a",
            codec,
            "-aframes",
            "2",
            "-f",
            "s16le",
            output.to_str().unwrap(),
        ]);
        let actual = std::fs::read(output).unwrap();
        let reference = ffmpeg(&[
            "-i", source, "-c:a", codec, "-aframes", "2", "-f", "s16le", "-",
        ]);
        assert_eq!(actual.len(), 8192 * 2 * 2);
        assert_eq!(actual, reference, "{codec}");
    }
}
