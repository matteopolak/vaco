//! Format-probing confusion sweep: for a range of formats the reference
//! `ffmpeg` binary can write, does `vaco-probe`'s own detector choose the
//! demuxer `ffprobe` chooses on the same bytes?
//!
//! # Why this exists
//!
//! A raw ADTS `.aac` file used to be misidentified as `cdgraphics` — not
//! because `cdgraphics` scored too high, but because nothing in the registry
//! claimed ADTS at all (see `vaco_demux_raw::aac`'s module docs). Per
//! `planning/AGENT-CONSTRAINTS.md`'s "first sightings are never the last",
//! that class of bug — a probe gap that a low, coincidental score wins by
//! default — was worth sweeping for siblings rather than declaring fixed
//! after the one reported case. This file is that sweep, kept as a checked-in
//! test rather than a one-off script so a future regression shows up in
//! `cargo test` instead of waiting for another bug report.
//!
//! # What it does *not* do
//!
//! It does not touch `divergences.toml`. That register is for a human,
//! CODEOWNERS-approved decision that a specific named difference is
//! acceptable (`docs/tool/vaco-conformance.md`); this file's own
//! `KNOWN_DIVERGENCES` list is a plain, reviewable Rust array recording
//! findings this sweep produced but nobody has adjudicated yet. Promoting one
//! into the governed register is a separate, deliberate step for whoever owns
//! that format.
//!
//! # Skips gracefully
//!
//! Same §1.5.4 contract as every other test in this crate: no `ffmpeg`/
//! `ffprobe` on `PATH`, or no `vaco-probe` built yet, and this prints why and
//! passes without asserting anything. `cargo test -- --nocapture` shows the
//! full confusion table either way.

#![expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use vaco_conformance::refbin::{self, Discovery, RefSpec};
use vaco_conformance::run::{self, Invocation};
use vaco_conformance::runner::UnderTest;

/// One format this sweep tries to synthesise and probe.
struct Case {
    /// Short, stable label for reports — not necessarily the format name
    /// either side reports.
    label: &'static str,
    /// `ffmpeg` arguments between `-i <lavfi source>` and the output path.
    /// The runner prepends `-nostdin -y -hide_banner -f lavfi -i <src>` and
    /// appends the output path itself.
    lavfi_src: &'static str,
    encode_args: &'static [&'static str],
    /// Extension the fixture is written with (drives both `ffmpeg`'s own
    /// muxer-by-extension guess where `encode_args` forces none, and the
    /// "with extension" half of each case).
    ext: &'static str,
    /// Also probe the same bytes under a `.bin` name, so a detector that only
    /// wins by extension is told apart from one that wins on content — the
    /// exact distinction `planning/00-decisions.md` calls out (content-match
    /// beating extension-match changed which code path ran).
    also_without_ext: bool,
}

const CASES: &[Case] = &[
    // ---------------------------------------------------- raw / headerless
    // These have the weakest magic and are where CLAUDE.md predicted scoring
    // bugs concentrate.
    Case {
        label: "aac-adts",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "aac", "-f", "adts"],
        ext: "aac",
        also_without_ext: true,
    },
    Case {
        label: "ac3",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "ac3", "-f", "ac3"],
        ext: "ac3",
        also_without_ext: true,
    },
    Case {
        label: "eac3",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "eac3", "-f", "eac3"],
        ext: "eac3",
        also_without_ext: true,
    },
    Case {
        label: "mp3",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "libmp3lame", "-f", "mp3"],
        ext: "mp3",
        also_without_ext: true,
    },
    Case {
        label: "h264-annexb",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx264", "-f", "h264"],
        ext: "h264",
        also_without_ext: true,
    },
    Case {
        label: "hevc-annexb",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx265", "-f", "hevc"],
        ext: "hevc",
        also_without_ext: true,
    },
    Case {
        // Standard broadcast rates only — `mpeg1video`/`mpeg2video` reject
        // the 5 fps every other case here uses (measured: `ffmpeg` refuses to
        // open the encoder at all).
        label: "mpeg1video",
        lavfi_src: "testsrc=size=64x64:rate=25:duration=1",
        encode_args: &["-c:v", "mpeg1video", "-f", "mpeg1video"],
        ext: "m1v",
        also_without_ext: true,
    },
    Case {
        label: "mpeg2video",
        lavfi_src: "testsrc=size=64x64:rate=25:duration=1",
        encode_args: &["-c:v", "mpeg2video", "-f", "mpeg2video"],
        ext: "m2v",
        also_without_ext: true,
    },
    Case {
        label: "mpeg4-m4v",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "mpeg4", "-f", "m4v"],
        ext: "m4v",
        also_without_ext: true,
    },
    Case {
        label: "mjpeg",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "mjpeg", "-f", "mjpeg"],
        ext: "mjpg",
        also_without_ext: true,
    },
    // -------------------------------------------------------- mpeg-ts family
    Case {
        label: "mpegts",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx264", "-c:a", "aac", "-f", "mpegts"],
        ext: "ts",
        also_without_ext: true,
    },
    Case {
        label: "mpegts-m2ts-ext",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx264", "-c:a", "aac", "-f", "mpegts"],
        ext: "m2ts",
        also_without_ext: false,
    },
    // ------------------------------------------------------------ containers
    Case {
        label: "mp4",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx264", "-c:a", "aac", "-f", "mp4"],
        ext: "mp4",
        also_without_ext: true,
    },
    Case {
        label: "matroska",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "libx264", "-c:a", "aac", "-f", "matroska"],
        ext: "mkv",
        also_without_ext: true,
    },
    Case {
        label: "avi",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "mpeg4", "-c:a", "libmp3lame", "-f", "avi"],
        ext: "avi",
        also_without_ext: true,
    },
    Case {
        label: "flv",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-c:v", "flv", "-c:a", "libmp3lame", "-f", "flv"],
        ext: "flv",
        also_without_ext: true,
    },
    Case {
        // `libvorbis` is not always built into `ffmpeg`; `libopus` is
        // required by `refspec.toml`'s configure line, so this exercises the
        // Ogg container path without depending on an optional encoder.
        label: "ogg-opus",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "libopus", "-f", "ogg"],
        ext: "ogg",
        also_without_ext: true,
    },
    Case {
        label: "wav",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-f", "wav"],
        ext: "wav",
        also_without_ext: true,
    },
    Case {
        label: "aiff",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-f", "aiff"],
        ext: "aiff",
        also_without_ext: true,
    },
    Case {
        label: "flac",
        lavfi_src: "sine=frequency=440:duration=1",
        encode_args: &["-c:a", "flac", "-f", "flac"],
        ext: "flac",
        also_without_ext: true,
    },
    Case {
        label: "gif",
        lavfi_src: "testsrc=size=64x64:rate=5:duration=1",
        encode_args: &["-f", "gif"],
        ext: "gif",
        also_without_ext: true,
    },
];

/// Names our detector and the reference's are both allowed to report for the
/// same content without that counting as a divergence: real aliasing, not a
/// bug. Each entry is `(case label, ours, theirs)`.
const ALIASES: &[(&str, &str, &str)] = &[
    // Measured: `ffmpeg -f lavfi ... -f mpeg1video`/`-f mpeg2video` both open
    // with the same `00 00 01 B3` sequence header and neither side can tell
    // MPEG-1 from MPEG-2 video from that alone; both name it "mpegvideo".
    ("mpeg1video", "mpegvideo", "mpegvideo"),
    ("mpeg2video", "mpegvideo", "mpegvideo"),
];

/// Divergences this sweep has found and recorded, but nobody with authority
/// over the affected format has adjudicated (`docs/tool/vaco-conformance.md`'s
/// register is the place for that once someone does). Each entry silences
/// exactly one `(case label, with/without extension)` pair so a *new*,
/// different divergence on the same case still fails loudly.
///
/// One real gap recorded here, found by this sweep's first run:
///
/// `gif` (both variants): the reference's `gif` demuxer reads all 5 frames of
/// an animated GIF; `vaco-demux-image2::pipe::DEMUXER_GIF` (registered as
/// `gif_pipe`, `extensions = "gif"`) is an image2-pipe demuxer built for a
/// *sequence of separate image files* fed through one pipe, and reads exactly
/// one frame from a single animated `.gif`'s own multi-frame container
/// structure. This is not a probe-scoring bug — `gif_pipe` is the only `gif`
/// demuxer registered, so there is nothing to mis-rank it against — it is a
/// missing demuxer: nothing here parses GIF89a's own frame sequencing
/// (Graphic Control Extension delays, disposal methods, the NETSCAPE2.0 loop
/// extension). That is a standalone clean-room implementation task, not a
/// scoring fix, so it is recorded here rather than attempted in this pass —
/// see `planning/TECH-DEBT.md` for the follow-up.
///
/// `mpegts-m2ts-ext` is deliberately **not** here even though this sweep
/// found it failing on the same run: that one *was* a probe-scoring bug
/// (`vaco_demux_raw::obu::looks_like_obu_stream` false-positiving on a BD-style
/// M2TS timecode prefix, beating `mpegts`'s correctly-earned 50 with an
/// accidental 51) and got fixed in the same commit as this comment, not
/// adjudicated as acceptable.
const KNOWN_DIVERGENCES: &[(&str, bool)] = &[("gif", false), ("gif", true)];

fn oracle() -> Option<vaco_conformance::refbin::Reference> {
    let spec = RefSpec::load().expect("refspec.toml loads");
    match refbin::discover(&spec) {
        Discovery::Found(r) => Some(*r),
        Discovery::Absent(why) => {
            println!("SKIPPED (no reference): {why}");
            None
        }
    }
}

fn probe_binary() -> Option<PathBuf> {
    let under_test = UnderTest::discover();
    if under_test.probe.is_none() {
        println!(
            "SKIPPED (vaco-probe not built): set VACO_BIN_PROBE or `cargo build -p vaco-probe`"
        );
    }
    under_test.probe
}

/// `-show_entries format=format_name -of default=nk=1:nw=1`, trimmed.
fn format_name(bin: &Path, file: &Path) -> Result<String, String> {
    let argv: Vec<String> = [
        "-v",
        "error",
        "-show_entries",
        "format=format_name",
        "-of",
        "default=nk=1:nw=1",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(file.to_string_lossy().into_owned()))
    .collect();
    let inv = Invocation::new(bin, argv).with_timeout(Duration::from_secs(20));
    let obs = run::run(&inv).map_err(|e| e.to_string())?;
    if !obs.succeeded() {
        return Err(format!(
            "{} exited {:?}: {}",
            bin.display(),
            obs.exit,
            obs.stderr_text()
        ));
    }
    Ok(obs.stdout_text().trim().to_owned())
}

/// `-count_packets -show_entries stream=nb_read_packets`, summed across every
/// stream — a demuxer that mis-detects the format almost always also
/// mis-counts packets (a lucky format-name match with a nonsense packet count
/// would still be a real bug), so this is the measurement half of "verify by
/// measuring", not just a nicety.
fn total_read_packets(bin: &Path, file: &Path) -> Result<u64, String> {
    let argv: Vec<String> = [
        "-v",
        "error",
        "-count_packets",
        "-show_entries",
        "stream=nb_read_packets",
        "-of",
        "default=nk=1:nw=1",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(file.to_string_lossy().into_owned()))
    .collect();
    let inv = Invocation::new(bin, argv).with_timeout(Duration::from_secs(30));
    let obs = run::run(&inv).map_err(|e| e.to_string())?;
    if !obs.succeeded() {
        return Err(format!(
            "{} exited {:?}: {}",
            bin.display(),
            obs.exit,
            obs.stderr_text()
        ));
    }
    obs.stdout_text()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.parse::<u64>().map_err(|e| e.to_string()))
        .sum()
}

fn is_aliased(label: &str, ours: &str, theirs: &str) -> bool {
    ours == theirs
        || ALIASES
            .iter()
            .any(|&(l, o, t)| l == label && o == ours && t == theirs)
}

fn is_known_divergence(label: &str, no_ext: bool) -> bool {
    KNOWN_DIVERGENCES
        .iter()
        .any(|&(l, n)| l == label && n == no_ext)
}

struct Row {
    entity: String,
    ours: String,
    theirs: String,
    ours_packets: Option<u64>,
    theirs_packets: Option<u64>,
    agrees: bool,
    allowed: bool,
}

#[test]
fn probe_choice_matches_the_reference_across_the_format_sweep() {
    let Some(reference) = oracle() else { return };
    let Some(probe) = probe_binary() else { return };

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut rows = Vec::new();
    let mut unexplained = Vec::new();

    for case in CASES {
        let fixture = tmp.path().join(format!("{}.{}", case.label, case.ext));
        let mut argv: Vec<String> = vec![
            "-nostdin".into(),
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            case.lavfi_src.into(),
        ];
        argv.extend(case.encode_args.iter().map(|s| (*s).to_owned()));
        argv.push(fixture.to_string_lossy().into_owned());
        let inv = Invocation::new(&reference.ffmpeg, argv).with_timeout(Duration::from_secs(60));
        let obs = match run::run(&inv) {
            Ok(o) => o,
            Err(e) => {
                println!("{}: could not launch ffmpeg: {e}", case.label);
                continue;
            }
        };
        if !obs.succeeded() {
            println!(
                "{}: SKIPPED — this ffmpeg build could not synthesise it: {}",
                case.label,
                obs.stderr_text().trim()
            );
            continue;
        }
        if !fixture.exists() {
            println!(
                "{}: SKIPPED — ffmpeg exited 0 but wrote nothing",
                case.label
            );
            continue;
        }

        let mut variants: Vec<(bool, PathBuf)> = vec![(false, fixture.clone())];
        if case.also_without_ext {
            let no_ext = tmp.path().join(format!("{}-noext", case.label));
            std::fs::copy(&fixture, &no_ext).expect("copy to extensionless variant");
            variants.push((true, no_ext));
        }

        for (no_ext, path) in variants {
            let entity = if no_ext {
                format!("{} (no ext)", case.label)
            } else {
                format!("{} (.{})", case.label, case.ext)
            };
            let theirs = match format_name(&reference.ffprobe, &path) {
                Ok(n) => n,
                Err(e) => {
                    println!("{entity}: reference ffprobe failed: {e}");
                    continue;
                }
            };
            let ours = match format_name(&probe, &path) {
                Ok(n) => n,
                Err(e) => {
                    println!("{entity}: vaco-probe failed: {e}");
                    format!("<error: {e}>")
                }
            };
            let theirs_packets = total_read_packets(&reference.ffprobe, &path).ok();
            let ours_packets = total_read_packets(&probe, &path).ok();

            let agrees = is_aliased(case.label, &ours, &theirs);
            let allowed = !agrees && is_known_divergence(case.label, no_ext);
            if !agrees && !allowed {
                unexplained.push(entity.clone());
            }
            rows.push(Row {
                entity,
                ours,
                theirs,
                ours_packets,
                theirs_packets,
                agrees,
                allowed,
            });
        }
    }

    // The confusion table. Always printed — `--nocapture` shows it whether
    // the test passes or not, which is the point: a measured list of every
    // probe divergence is valuable on its own, per the brief.
    println!(
        "\n{:<28} {:<16} {:<16} {:>12} {:>12}  status",
        "case", "ours", "reference", "ours pkts", "ref pkts"
    );
    for r in &rows {
        let status = if r.agrees {
            "agree"
        } else if r.allowed {
            "known"
        } else {
            "DIVERGED"
        };
        println!(
            "{:<28} {:<16} {:<16} {:>12} {:>12}  {status}",
            r.entity,
            r.ours,
            r.theirs,
            r.ours_packets.map_or("N/A".to_owned(), |n| n.to_string()),
            r.theirs_packets.map_or("N/A".to_owned(), |n| n.to_string()),
        );
    }
    assert!(
        !rows.is_empty(),
        "the sweep produced no comparable cases at all"
    );

    assert!(
        unexplained.is_empty(),
        "unexplained probe divergences (see the table above): {unexplained:?}"
    );

    // The specific regression this file exists to guard: a bare ADTS file,
    // named or not, must be chosen as `aac`, and its packet count must be in
    // the same order of magnitude as the reference's (not the ~760-vs-88
    // mismatch a `cdgraphics` misdetection produced before the fix, which is
    // off by roughly 9x on this fixture — any factor-of-several disagreement
    // is already conclusive here, so a loose bound keeps this from being
    // sensitive to exactly how many frames a 1 s sine tone happens to encode
    // to on a future encoder version).
    for r in &rows {
        if r.entity.starts_with("aac-adts") {
            assert_eq!(r.ours, "aac", "{}: expected our own aac demuxer", r.entity);
            if let (Some(ours), Some(theirs)) = (r.ours_packets, r.theirs_packets) {
                let ratio = ours.max(theirs) as f64 / ours.min(theirs).max(1) as f64;
                assert!(
                    ratio < 2.0,
                    "{}: packet counts disagree by {ratio:.1}x (ours={ours}, reference={theirs})",
                    r.entity
                );
            }
        }
    }
}
