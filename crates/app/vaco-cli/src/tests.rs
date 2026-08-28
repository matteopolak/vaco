//! Whole-invocation tests: argv in, stderr text and an exit code out.
//!
//! Most of what is here predates the container wave and still checks exactly
//! what it always did — **stream selection**, **stderr text and exit code**,
//! and **packet counts through the pipeline**, all reachable without ever
//! looking at a file on disk. That was the *whole* acceptance surface when
//! there were no muxers to write one; now it is the surface every test that
//! does not care about container bytes should keep using; see
//! `an_actual_muxer_writes_bytes_a_prober_can_read_back` below for the one
//! that does look at a real file.
//!
//! The fixtures are built with `vaco_demux_matroska::synth`, a dev-dependency.
//! It is not a muxer — it writes exactly what it is told, including per-track
//! `FlagDefault`, which is the field the whole auto-selection rule turns on and
//! which `ffmpeg`'s own Matroska muxer will not let a test control.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::io::Write as _;

use vaco_demux_matroska::ebml::schema as el;
use vaco_demux_matroska::synth::{self, SegmentSize};
use vaco_format_core::Demuxer as _;

use crate::{ExitCode, run};

// ------------------------------------------------------------------ fixtures

fn info(scale: u64) -> Vec<u8> {
    let mut b = synth::uint(el::TIMESTAMPSCALE, scale);
    b.extend_from_slice(&synth::string(el::MUXINGAPP, "vaco-cli-test"));
    b
}

fn track(number: u64, kind: u64, codec: &str, inner: &[u8], default: bool) -> Vec<u8> {
    let mut body = synth::uint(el::TRACKNUMBER, number);
    body.extend_from_slice(&synth::uint(el::TRACKUID, number));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, kind));
    body.extend_from_slice(&synth::string(el::CODECID, codec));
    body.extend_from_slice(&synth::uint(el::FLAGDEFAULT, u64::from(default)));
    body.extend_from_slice(inner);
    synth::element(el::TRACKENTRY, &body)
}

fn video(number: u64, w: u64, h: u64, default: bool) -> Vec<u8> {
    let mut v = synth::uint(el::PIXELWIDTH, w);
    v.extend_from_slice(&synth::uint(el::PIXELHEIGHT, h));
    track(number, 1, "V_VP8", &synth::element(el::VIDEO, &v), default)
}

fn audio(number: u64, channels: u64, default: bool) -> Vec<u8> {
    let mut a = synth::float(el::SAMPLINGFREQUENCY, 48_000.0);
    a.extend_from_slice(&synth::uint(el::CHANNELS, channels));
    track(number, 2, "A_OPUS", &synth::element(el::AUDIO, &a), default)
}

fn block(track_number: u8, ts: i16, payload: &[u8]) -> Vec<u8> {
    synth::element(
        el::SIMPLEBLOCK,
        &synth::block_body(track_number, ts, 0x80, payload),
    )
}

/// Two video tracks and two audio tracks, shaped exactly like the file the
/// reference was measured on: the *smaller* video and the *fewer*-channel audio
/// carry `default`.
///
/// ```text
/// #0  video 320x240  default
/// #1  video 640x480
/// #2  audio 2ch      default
/// #3  audio 6ch
/// ```
///
/// `ffmpeg -i <equivalent>.mkv -f null -` selects `#0:0` and `#0:2`.
fn four_track_file() -> Vec<u8> {
    let mut tracks = video(1, 320, 240, true);
    tracks.extend_from_slice(&video(2, 640, 480, false));
    tracks.extend_from_slice(&audio(3, 2, true));
    tracks.extend_from_slice(&audio(4, 6, false));

    let mut children = Vec::new();
    for i in 0..4i16 {
        children.push(block(1, i, &[0xAA; 100]));
        children.push(block(2, i, &[0xBB; 200]));
        children.push(block(3, i, &[0xCC; 30]));
        children.push(block(4, i, &[0xDD; 60]));
    }
    let cluster = synth::cluster(0, &children, SegmentSize::Known);
    synth::file(
        "matroska",
        &info(1_000_000),
        &tracks,
        &[cluster],
        SegmentSize::Known,
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: String,
}

fn fixture(bytes: &[u8]) -> Fixture {
    fixture_named("in.mkv", bytes)
}

fn fixture_named(name: &str, bytes: &[u8]) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(bytes).expect("write");
    f.sync_all().expect("sync");
    let path = path.to_string_lossy().into_owned();
    Fixture { _dir: dir, path }
}

// ------------------------------------------------------------------- harness

struct Outcome {
    code: ExitCode,
    out: String,
    err: String,
}

impl Outcome {
    /// stderr with the banner line removed, so a test can assert on the message
    /// without pinning the version string.
    fn message(&self) -> String {
        self.err
            .lines()
            .filter(|l| !l.starts_with("vaco version "))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            })
    }
}

fn go(argv: &[&str]) -> Outcome {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(argv, &mut out, &mut err);
    Outcome {
        code,
        out: String::from_utf8_lossy(&out).into_owned(),
        err: String::from_utf8_lossy(&err).into_owned(),
    }
}

// -------------------------------------------------------------- usage errors

#[test]
fn no_arguments_exits_one() {
    // OBSERVED: bare `ffmpeg` exits 1.
    let r = go(&[]);
    assert_eq!(r.code.code(), 1);
    assert!(r.err.contains("usage: vaco"), "{}", r.err);
}

#[test]
fn an_input_with_no_output_exits_one() {
    // OBSERVED: `ffmpeg -i multi.mkv` prints the `Input #0` dump (#641), then
    // this exact line, and exits 1. A nonexistent file used to stand in here
    // — harmless only because the "no output" check ran *before* any input
    // was opened, which was itself the gap #641 reports: the reference opens
    // and dumps the input first. Now that the order matches, this needs an
    // input that actually opens.
    let f = fixture(&four_track_file());
    let r = go(&["-i", &f.path]);
    assert_eq!(r.code.code(), 1, "{}", r.message());
    assert!(
        r.message().starts_with("Input #0, matroska"),
        "{}",
        r.message()
    );
    assert!(
        r.message()
            .ends_with("At least one output file must be specified\n"),
        "{}",
        r.message()
    );
}

#[test]
fn an_unrecognised_option_exits_eight() {
    // OBSERVED: `ffmpeg -qwerty 3 -i x -f null -` exits 8.
    let r = go(&["-qwerty", "3", "-i", "x.mkv", "-f", "null", "-"]);
    assert_eq!(r.code.code(), 8);
    assert_eq!(
        r.message(),
        "Unrecognized option 'qwerty'.\nError splitting the argument list: Option not found\n"
    );
}

#[test]
fn a_missing_input_exits_two_hundred_and_fifty_four() {
    // OBSERVED: `ffmpeg -i nope.mkv -f null -` exits 254 (`ENOENT` truncated).
    let r = go(&["-i", "/nonexistent/vaco-cli-test.mkv", "-f", "null", "-"]);
    assert_eq!(r.code.code(), 254);
    assert_eq!(
        r.message(),
        "[in#0] Error opening input: No such file or directory\n\
         Error opening input file /nonexistent/vaco-cli-test.mkv.\n\
         Error opening input files: No such file or directory\n"
    );
}

#[test]
fn a_file_that_is_not_a_container_exits_one_hundred_and_eighty_three() {
    // OBSERVED: `ffmpeg -i script.sh -f null -` exits 183, the low byte of
    // AVERROR_INVALIDDATA.
    let f = fixture(b"this is not a media file, not even slightly\n");
    let r = go(&["-i", &f.path, "-f", "null", "-"]);
    assert_eq!(r.code.code(), 183, "{}", r.err);
    assert!(
        r.message()
            .contains("Error opening input files: Invalid data found when processing input"),
        "{}",
        r.message()
    );
}

#[test]
fn the_banner_precedes_the_error_and_hide_banner_removes_it() {
    // OBSERVED: `ffmpeg -qwerty 3` prints the banner first, then the error.
    let r = go(&["-qwerty", "3"]);
    assert!(r.err.starts_with("vaco version "), "{}", r.err);
    let r = go(&["-hide_banner", "-qwerty", "3"]);
    assert!(!r.err.contains("vaco version "), "{}", r.err);
}

// ------------------------------------------------------------------- listing

#[test]
fn version_exits_zero_and_prints_to_stdout() {
    let r = go(&["-hide_banner", "-version"]);
    assert_eq!(r.code, ExitCode::OK);
    assert!(r.out.starts_with("vaco version "), "{:?}", r.out);
}

#[test]
fn formats_lists_demuxers_and_muxers_by_what_the_registry_actually_has() {
    // Named "admits to no muxers" until the container wave landed 63 of
    // them; the mapping this test should pin is "a format shows `E` exactly
    // when `vaco_registry::muxer_by_name` finds it", not a count.
    let r = go(&["-hide_banner", "-formats"]);
    assert_eq!(r.code, ExitCode::OK);
    assert!(r.out.contains("matroska"), "{}", r.out);
    let Some(name) = vaco_registry::muxers().first().map(|m| m.name) else {
        return;
    };
    assert!(
        r.out.lines().any(|l| l.contains('E') && l.contains(name)),
        "{name} is a registered muxer but -formats does not mark it E:\n{}",
        r.out
    );
}

// -------------------------------------------------------------------- -h end-to-end

#[test]
fn bare_h_exits_zero_through_the_whole_binary() {
    // Every `-h` form exits 0 in the reference (measured, ffmpeg 8.1, no
    // pipe) — this is the same exit-code family the listing commands share,
    // asserted here through the real argv/exit-code path rather than only
    // against `help::render` directly.
    for argv in [
        vec!["-hide_banner", "-h"],
        vec!["-hide_banner", "-h", "long"],
        vec!["-hide_banner", "-h", "full"],
        vec!["-hide_banner", "-h", "decoder=h264"],
        vec!["-hide_banner", "-h", "protocol=file"],
        vec!["-hide_banner", "-h", "bogus"],
        vec!["-hide_banner", "-?"],
        vec!["-hide_banner", "-help"],
        vec!["-hide_banner", "--help"],
    ] {
        let r = go(&argv);
        assert_eq!(r.code, ExitCode::OK, "{argv:?}");
        assert!(
            r.out.ends_with("Exiting with exit code 0\n"),
            "{argv:?}: {}",
            r.out
        );
    }
}

#[test]
fn h_swallows_the_next_token_even_when_it_looks_like_an_option() {
    // Measured: `ffmpeg -h -i x` reports `Unknown help option '-i'.` and `x`
    // is never looked at — no "missing output file" failure follows.
    let r = go(&["-hide_banner", "-h", "-i", "x"]);
    assert_eq!(r.code, ExitCode::OK, "{:?}", r.out);
    assert!(
        r.out.starts_with("Unknown help option '-i'.\n"),
        "{}",
        r.out
    );
}

// ------------------------------------------------- reading without writing

#[test]
fn an_output_this_build_can_read_but_not_write_says_so() {
    // The extension comes from the registry rather than being written in:
    // this test named `mkv`, and the day the Matroska muxer landed it failed
    // — on success. What it is really about is the wording used whenever a
    // read-only format is asked to be an output.
    let Some(ext) = vaco_registry::demuxers()
        .iter()
        .filter(|d| vaco_registry::muxer_by_name(d.name).is_none())
        .filter_map(|d| d.extensions.first().copied())
        .find(|e| {
            vaco_registry::muxers_for_extension(&format!("x.{e}"))
                .next()
                .is_none()
        })
    else {
        return;
    };
    let f = fixture(&four_track_file());
    let out = format!("out.{ext}");
    let r = go(&["-i", &f.path, "-c", "copy", &out]);
    assert_eq!(r.code.code(), 8, "{ext}: {}", r.message());
    assert!(
        r.message()
            .contains("reads that format but cannot write it"),
        "{}",
        r.message()
    );
    assert!(
        r.message()
            .ends_with("Error opening output files: Muxer not found\n")
    );
}

#[test]
fn an_output_format_nothing_claims_keeps_the_reference_wording() {
    // OBSERVED: exit 234, and this exact line modulo the log pointer. It is
    // no longer the *first* line of output — #641's `Input #0` dump now
    // precedes it here too, since the input opens fine before output
    // resolution fails.
    let f = fixture(&four_track_file());
    let r = go(&["-i", &f.path, "-c", "copy", "-f", "nosuchformat", "-"]);
    assert_eq!(r.code.code(), 234);
    assert!(
        r.message()
            .contains("[AVFormatContext] Requested output format 'nosuchformat' is not known.\n"),
        "{}",
        r.message()
    );
}

#[test]
fn an_output_with_no_c_copy_takes_the_missing_encoder_path() {
    // The reference's own message for a build without the encoder it wants,
    // which is exactly this build's situation. OBSERVED exit 8.
    let f = fixture(&four_track_file());
    let r = go(&["-i", &f.path, "-f", "null", "-"]);
    assert_eq!(r.code.code(), 8, "{}", r.message());
    assert!(
        r.message().contains("is probably disabled"),
        "{}",
        r.message()
    );
    assert!(
        r.message()
            .ends_with("Error opening output files: Encoder not found\n")
    );
}

// --------------------------------------------------------- the working spine

#[test]
fn the_default_selection_matches_the_reference_on_a_four_track_file() {
    // The whole point. `ffmpeg -i <this shape> -f null -` maps #0:0 and #0:2 —
    // the *smaller* video and the *fewer*-channel audio, because both carry
    // `default` and the flag is worth 5 000 000 pixels.
    let f = fixture(&four_track_file());
    let r = go(&["-i", &f.path, "-c", "copy", "-f", "null", "-"]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains(
            "Stream mapping:\n  Stream #0:0 -> #0:0 (copy)\n  Stream #0:2 -> #0:1 (copy)\n"
        ),
        "{}",
        r.message()
    );
}

#[test]
fn packets_reach_the_sink_and_are_counted() {
    // Four blocks per track: 4 x 100 video bytes and 4 x 30 audio bytes on the
    // two selected streams.
    let f = fixture(&four_track_file());
    let cli = crate::cli::parse(&["-i", &f.path, "-c", "copy", "-f", "null", "-"]).unwrap();
    let inputs: Vec<_> = cli
        .inputs
        .iter()
        .map(|s| {
            crate::input::open(s.index, &s.url, &crate::input::OpenRequest::default()).unwrap()
        })
        .collect();
    let files: Vec<_> = inputs.iter().map(crate::exec::describe).collect();
    let mut used_complex = std::collections::HashSet::new();
    let outputs: Vec<_> = cli
        .outputs
        .iter()
        .map(|o| crate::exec::resolve_output(&cli, o, &files, &[], &mut used_complex).unwrap())
        .collect();
    let report =
        crate::exec::run_pipeline(inputs, &outputs, &files, &cli.complex_filters, true).unwrap();

    let tally = &report.tallies[0];
    assert!(tally.header_written && tally.trailer_written);
    assert_eq!(tally.streams.len(), 2);
    assert_eq!(tally.streams[0].packets, 4);
    assert_eq!(tally.streams[0].bytes, 400);
    assert_eq!(tally.streams[1].packets, 4);
    assert_eq!(tally.streams[1].bytes, 120);
    assert_eq!(tally.packets(), 8);
}

/// A one-track file whose presentation timestamps are reordered, as every
/// B-frame video stream's are. The codec is `V_VP8`, which the demuxer knows
/// does **not** reorder — that is the point of this fixture.
fn reordered_pts_file() -> Vec<u8> {
    let tracks = video(1, 320, 240, true);
    let children: Vec<Vec<u8>> = [0i16, 160, 80, 40, 120]
        .into_iter()
        .map(|ts| block(1, ts, &[0xAA; 64]))
        .collect();
    let cluster = synth::cluster(0, &children, SegmentSize::Known);
    synth::file(
        "matroska",
        &info(1_000_000),
        &tracks,
        &[cluster],
        SegmentSize::Known,
    )
}

/// The repair path, pinned because it is silent and produces invalid values.
///
/// `vaco_format_core::time::DemuxTimestamps::fix` rule R19 gives a
/// non-reordering codec `dts = pts` for free, and R22 then repairs the
/// resulting non-monotonic sequence by bumping. Observed on this fixture, whose
/// PTS run `0, 160, 80, 40, 120`:
///
/// ```text
/// pts=0    dts=0
/// pts=160  dts=160
/// pts=80   dts=161     <- repaired, and now greater than its own PTS
/// pts=40   dts=162
/// pts=120  dts=242
/// ```
///
/// The run therefore succeeds, and the timestamps the sink sees are fiction.
/// `dts > pts` is not a valid packet in any container. Reported against
/// `vaco-format-core`; this test exists so that fixing R22 shows up here rather
/// than as a mysterious new failure in the CLI.
#[test]
fn a_reordered_pts_sequence_on_a_non_reordering_codec_is_repaired_not_refused() {
    let f = fixture(&reordered_pts_file());
    let r = go(&["-i", &f.path, "-c", "copy", "-f", "null", "-"]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains("  Stream #0:0 -> #0:0 (copy)\n"),
        "{}",
        r.message()
    );
}

/// A monotonic sequence of the same shape, so the test above is a statement
/// about the *repair* and not about video in general.
#[test]
fn monotonic_video_streamcopies_end_to_end() {
    let tracks = video(1, 320, 240, true);
    let children: Vec<Vec<u8>> = [0i16, 40, 80, 120, 160]
        .into_iter()
        .map(|ts| block(1, ts, &[0xAA; 64]))
        .collect();
    let cluster = synth::cluster(0, &children, SegmentSize::Known);
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &tracks,
        &[cluster],
        SegmentSize::Known,
    );
    let f = fixture(&bytes);
    let r = go(&["-i", &f.path, "-c", "copy", "-f", "null", "-"]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains("  Stream #0:0 -> #0:0 (copy)\n"),
        "{}",
        r.message()
    );
}

/// The one test in this file that leaves the packet-count/stderr surface and
/// looks at real bytes on disk: remux the same fixture `monotonic_video_
/// streamcopies_end_to_end` uses, but to a real `.mkv` this time, then read
/// the *result* back through a second, independent invocation of the whole
/// binary.
///
/// This is what makes `exec::muxer_for` reaching the registry an observable
/// fact rather than an implementation detail: before this pass, `-f matroska
/// out.mkv` exited 0, printed a plausible summary and `out.mkv` did not
/// exist (`planning/CONFORMANCE-FINDINGS.md` #6). A test that only checks the
/// exit code and the stderr text cannot tell that apart from a real remux —
/// only opening the file the run claims to have written can.
#[test]
fn an_actual_muxer_writes_bytes_a_prober_can_read_back() {
    let f = fixture(&four_track_file());
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("out.mkv");
    let out_str = out_path.to_str().expect("utf8 tempdir path").to_owned();

    let r = go(&["-i", &f.path, "-c", "copy", "-f", "matroska", &out_str]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains(
            "Stream mapping:\n  Stream #0:0 -> #0:0 (copy)\n  Stream #0:2 -> #0:1 (copy)\n"
        ),
        "{}",
        r.message()
    );
    // `[out#0/matroska] video:0KiB audio:0KiB … muxing overhead: N%` — the
    // 460-byte fixture rounds to 0KiB either side, so the number that matters
    // here is the overhead figure: `unknown` would mean nothing was measured
    // as actually written (the pre-fix behaviour, on a NullMuxer path this
    // format no longer takes).
    assert!(r.message().contains("[out#0/matroska]"), "{}", r.message());
    assert!(
        !r.message().contains("muxing overhead: unknown"),
        "a real container's overhead must be measured, not unknown: {}",
        r.message()
    );

    // A real file, not empty, not the `-f null` sink's silent nothing.
    let meta = std::fs::metadata(&out_path).expect("the remux must have created a real file");
    assert!(meta.len() > 0, "a remuxed file must not be empty");

    // Read it back: a second, independent invocation, through the same
    // protocol → probe → demux → discovery → selection spine the first run
    // used to write it. `-f null -` on the *output* of the first run selects
    // the same two streams `four_track_file()` was built to make `vaco`
    // pick on the *input* — a mismatch here means the bytes on disk are not
    // what the first run claimed to write.
    let r2 = go(&["-i", &out_str, "-c", "copy", "-f", "null", "-"]);
    assert_eq!(r2.code, ExitCode::OK, "{}", r2.message());
    assert!(
        r2.message().contains(
            "Stream mapping:\n  Stream #0:0 -> #0:0 (copy)\n  Stream #0:1 -> #0:1 (copy)\n"
        ),
        "{}",
        r2.message()
    );
    assert!(
        r2.message().contains("video:0KiB audio:0KiB"),
        "the round trip must carry the same payload back: {}",
        r2.message()
    );
}

#[test]
fn map_selects_exactly_what_it_names_and_in_order() {
    let f = fixture(&four_track_file());
    let r = go(&[
        "-i", &f.path, "-map", "0:a", "-map", "0:v", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    let m = r.message();
    let mapped: Vec<&str> = m.lines().filter(|l| l.contains(" -> ")).collect();
    assert_eq!(
        mapped,
        vec![
            "  Stream #0:2 -> #0:0 (copy)",
            "  Stream #0:3 -> #0:1 (copy)",
            "  Stream #0:0 -> #0:2 (copy)",
            "  Stream #0:1 -> #0:3 (copy)",
        ]
    );
}

#[test]
fn a_map_that_matches_nothing_exits_two_hundred_and_thirty_four() {
    let f = fixture(&four_track_file());
    let r = go(&[
        "-i", &f.path, "-map", "0:s", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code.code(), 234);
    assert!(
        r.message().contains("Stream map '' matches no streams.\n"),
        "{}",
        r.message()
    );
    // With `?` the same map is a silent no-op, and because no positive map
    // matched anything the whole output is dropped rather than reported empty.
    // OBSERVED: `ffmpeg -i multi.mkv -map 0:s? -c copy -f null -` exits 0, and
    // the same run to `-f matroska /tmp/o.mkv` creates no file at all.
    let r = go(&[
        "-i", &f.path, "-map", "0:s?", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        !r.message().contains("does not contain any stream"),
        "{}",
        r.message()
    );
}

#[test]
fn an_output_emptied_by_a_negative_map_is_an_error_not_a_drop() {
    // The other side of the same rule: a positive map *did* match, so the empty
    // result is reported rather than the file being dropped. OBSERVED exit 234.
    let f = fixture(&four_track_file());
    let r = go(&[
        "-i", &f.path, "-map", "0:v:0", "-map", "-0:v:0", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code.code(), 234, "{}", r.message());
    assert!(
        r.message()
            .contains("[out#0/null] Output file does not contain any stream\n"),
        "{}",
        r.message()
    );
}

#[test]
fn dropping_every_type_leaves_an_output_with_no_streams() {
    // OBSERVED: `ffmpeg -i multi.mkv -vn -an -sn -dn -f null -` exits 234.
    let f = fixture(&four_track_file());
    let r = go(&[
        "-i", &f.path, "-vn", "-an", "-sn", "-dn", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code.code(), 234);
    assert!(
        r.message()
            .contains("[out#0/null] Output file does not contain any stream\n"),
        "{}",
        r.message()
    );
}

#[test]
fn two_inputs_are_addressable_by_index() {
    let a = fixture(&four_track_file());
    let b = fixture(&four_track_file());
    let r = go(&[
        "-i", &a.path, "-i", &b.path, "-map", "1:v:1", "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains("  Stream #1:1 -> #0:0 (copy)\n"),
        "{}",
        r.message()
    );
}

#[test]
fn a_forced_input_format_skips_probing() {
    let f = fixture(&four_track_file());
    let r = go(&[
        "-f", "matroska", "-i", &f.path, "-c", "copy", "-f", "null", "-",
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
}

#[test]
fn a_protocol_whitelist_that_excludes_file_blocks_the_input() {
    let f = fixture(&four_track_file());
    let r = go(&[
        "-protocol_whitelist",
        "http",
        "-i",
        &f.path,
        "-c",
        "copy",
        "-f",
        "null",
        "-",
    ]);
    assert!(!r.code.is_ok(), "{}", r.message());
    assert!(
        r.message().contains("Error opening input file"),
        "{}",
        r.message()
    );
}

// ---------------------------------------------------------------- properties

/// CL-16, end to end: `-metadata`/`-metadata:s:v:0` on a real remux, read
/// back through `vaco_demux_matroska::MatroskaDemuxer` — the same demuxer
/// `vaco-probe` uses via the registry, so this is that binary's own read
/// path, exercised in-process rather than by shelling out to it. The "best
/// test" the CL-16 brief asks for, at the CLI layer rather than the muxer
/// unit-test layer `vaco-mux-matroska`/`vaco-mux-mp4` already cover.
#[test]
fn metadata_options_reach_a_real_remuxed_file() {
    let f = fixture(&four_track_file());
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("out.mkv");
    let out_str = out_path.to_str().expect("utf8 tempdir path").to_owned();

    // The fixture auto-selects video track #0 and audio track #2 (see
    // `four_track_file`'s docs), which land at output positions `v:0`/`a:0`.
    let r = go(&[
        "-i",
        &f.path,
        "-metadata",
        "title=Integration Title",
        "-metadata:s:v:0",
        "language=eng",
        "-c",
        "copy",
        "-f",
        "matroska",
        &out_str,
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());

    let bytes = std::fs::read(&out_path).expect("read the remuxed file back");
    let src: Box<dyn vaco_io::MediaSource> = Box::new(vaco_io::MemorySource::new(bytes));
    let demux = vaco_demux_matroska::MatroskaDemuxer::open(
        src,
        &vaco_format_core::discovery::NoParsers,
        &vaco_format_core::FormatOptions::default(),
    )
    .expect("the remuxed file must be a valid matroska file");

    assert!(
        demux
            .metadata()
            .iter()
            .any(|(k, v)| k == "title" && v == "Integration Title"),
        "global -metadata must reach the file: {:?}",
        demux.metadata()
    );

    let video = demux
        .streams()
        .iter()
        .find(|s| s.params.media_type == Some(vaco_core::MediaType::Video))
        .expect("the copied video stream");
    assert!(
        video
            .metadata
            .iter()
            .any(|(k, v)| k == "language" && v == "eng"),
        "-metadata:s:v:0 must reach exactly the video track: {:?}",
        video.metadata
    );
}

#[test]
fn every_run_terminates_and_never_panics_on_arbitrary_argv() {
    // The narrow, deterministic companion to the `cli_run` fuzz target: a fixed
    // set of shapes that have historically broken argv handling.
    let cases: &[&[&str]] = &[
        &[],
        &["-"],
        &["--"],
        &["-i"],
        &["-i", "-"],
        &["-i", "--", "-f", "null", "-"],
        &["-map"],
        &["-map", ""],
        &["-map", "["],
        &["-map", "-"],
        &["-f", "", "-i", "", ""],
        &["-c:v:v:v", "copy"],
        &["-i", "/dev/null", "-f", "null", "-"],
        &["-nostats", "-nostdin", "-y", "-n"],
        &["-loglevel"],
        &["-h"],
        &["-h", "full"],
    ];
    for argv in cases {
        let r = go(argv);
        // Any code at all is fine; hanging or panicking is not.
        let _ = r.code.code();
    }
}

// --------------------------------------------------------- CL-25 filter_complex

/// An 8x8 solid-colour PNG, generated by real `ffmpeg` (D6: measuring a
/// shipped binary's output is clean-room, and this is pixel data, not
/// expression) — small enough to embed, and enough to exercise a real
/// decode → filter → encode → mux leg end to end.
const TINY_PNG: &[u8] = include_bytes!("../testdata/tiny.png");

#[test]
fn filter_complex_map_label_produces_a_real_scaled_output_file() {
    // The end-to-end case CL-25's brief asks for: `-filter_complex` with a
    // real link label, `-map [label]` pulling from it, driven all the way to
    // a real file — not just `complexgraph`'s own unit tests, which stop at
    // resolving labels and running the graph in isolation.
    let f = fixture_named("in.png", TINY_PNG);
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("out.png");
    let out_str = out_path.to_str().expect("utf8 tempdir path").to_owned();

    let r = go(&[
        "-y",
        "-f",
        "png_pipe",
        "-i",
        &f.path,
        "-filter_complex",
        "[0:v]scale=4:4[out]",
        "-map",
        "[out]",
        "-c:v",
        "png",
        "-f",
        "image2",
        &out_str,
    ]);
    assert_eq!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains("Stream #complex:0 -> #0:0 (png)"),
        "{}",
        r.message()
    );

    let bytes = std::fs::read(&out_path).expect("the run must have created a real file");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG: {bytes:?}");
    // IHDR's width/height, big-endian u32 at fixed offsets — the structural
    // check that the `scale` filter actually ran rather than the muxer
    // silently reusing the 8x8 source.
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    assert_eq!((width, height), (4, 4), "scale=4:4 did not resize the output");
}

#[test]
fn c_copy_on_a_complex_filtergraph_output_is_a_hard_error() {
    // Measured (`ffmpeg 8.1`, `-map '[out]' -c copy`): exit 234, "Streamcopy
    // requested for output stream fed from a complex filtergraph."
    let f = fixture_named("in.png", TINY_PNG);
    let r = go(&[
        "-f",
        "png_pipe",
        "-i",
        &f.path,
        "-filter_complex",
        "[0:v]scale=4:4[out]",
        "-map",
        "[out]",
        "-c",
        "copy",
        "-f",
        "image2",
        "/dev/null",
    ]);
    assert_eq!(r.code.code(), 234, "{}", r.message());
    assert!(
        r.message().contains(
            "Streamcopy requested for output stream fed from a complex filtergraph."
        ),
        "{}",
        r.message()
    );
}

#[test]
fn an_unconsumed_complex_output_label_is_a_hard_error() {
    let f = fixture_named("in.png", TINY_PNG);
    let r = go(&[
        "-f",
        "png_pipe",
        "-i",
        &f.path,
        "-filter_complex",
        "[0:v]scale=4:4[out]",
        "-c:v",
        "png",
        "-f",
        "image2",
        "/dev/null",
    ]);
    assert_ne!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains("'out' is not connected"),
        "{}",
        r.message()
    );
}

#[test]
fn a_complex_output_label_used_twice_is_the_references_own_wording() {
    let f = fixture_named("in.png", TINY_PNG);
    let r = go(&[
        "-f",
        "png_pipe",
        "-i",
        &f.path,
        "-filter_complex",
        "[0:v]scale=4:4[out]",
        "-map",
        "[out]",
        "-map",
        "[out]",
        "-c:v",
        "png",
        "-c:v",
        "png",
        "-f",
        "image2",
        "/dev/null",
    ]);
    assert_ne!(r.code, ExitCode::OK, "{}", r.message());
    assert!(
        r.message().contains(
            "Output with label 'out' does not exist in any defined filter graph, or was already used elsewhere."
        ),
        "{}",
        r.message()
    );
}
