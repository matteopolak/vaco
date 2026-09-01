//! The case runner (plan 13 §1.9).
//!
//! # What it is
//!
//! Takes expanded cases, runs both binaries under [`crate::run`], and produces
//! verdicts. Each case gets its own temp dir and shares no mutable state with
//! any other, which is what makes them independently re-runnable and safely
//! parallelisable later.
//!
//! # Graceful absence
//!
//! Two things can be missing, and neither is a failure:
//!
//! - **The reference.** `cargo test` must pass on a machine without `FFmpeg`
//!   (§1.5.4), so a missing reference skips every case with the message
//!   [`crate::refbin::discover`] produced.
//! - **The binary under test.** `vaco-probe` and `vaco` do not exist yet. A
//!   case whose tool is not built skips with a message naming the binary and
//!   the environment variable that would point at it.
//!
//! Skips are **counted and reported**, never silent. §1.5.4 gives a tier a skip
//! budget for exactly this reason: coverage that erodes quietly is worse than
//! coverage that was never claimed.
//!
//! # How to change it
//!
//! Parallelism goes here: cases are independent by construction, so a
//! work-stealing pool over [`Runner::run_case`] is a drop-in change. It is
//! deliberately not written yet — with no binary under test there is nothing to
//! parallelise, and an untested thread pool in the harness would be a source of
//! flakes rather than speed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::case::{Case, Compare, FailureKind, SkipReason, Tier, Tool, Verdict};
use crate::compare::{self, Pair};
use crate::divergence::Allowlist;
use crate::refbin::Reference;
use crate::run::{Invocation, run};

/// Synthesised media, produced once per process and reused across cases.
///
/// Without the cache, a suite of a thousand cases over four media would invoke
/// the reference four thousand times to build the same four files. The key is
/// the generating argument vector, so two suites asking for the same media
/// share one file — and two suites asking for *nearly* the same media do not,
/// which is the behaviour you want when the difference is the point.
#[derive(Debug, Default)]
pub struct MediaCache {
    dir: Option<std::sync::Arc<tempfile::TempDir>>,
    built: std::cell::RefCell<std::collections::BTreeMap<String, PathBuf>>,
}

impl MediaCache {
    /// Materialise `media` and return the path to it.
    ///
    /// # Errors
    /// A message naming the media and what the reference said.
    pub fn path(
        &self,
        media: &crate::case::MediaRef,
        reference: &std::path::Path,
    ) -> Result<PathBuf, String> {
        if let Some(name) = media.source.strip_prefix("corpus://") {
            return self.corpus_path(name);
        }
        let Some(argv) = &media.generate else {
            return Err(format!(
                "media `{}`: source `{}` is not a path this build can resolve, and \
                 the entry declares no `generate`. A `corpus://<name>` source needs a \
                 matching entry in vaco-corpus's vaco-media.lock; anything else needs a \
                 `generate` command the reference can synthesise it with.",
                media.id, media.source
            ));
        };
        let key = format!("{}\u{1}{}", media.file_name(), argv.join("\u{1}"));
        if let Some(hit) = self.built.borrow().get(&key) {
            return Ok(hit.clone());
        }
        let dir = self
            .dir
            .as_ref()
            .ok_or("the media cache has no directory")?;
        // One subdirectory per media so two entries may share a file name.
        let sub = dir.path().join(format!("m{}", self.built.borrow().len()));
        std::fs::create_dir_all(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
        let out = sub.join(media.file_name());

        let mut full: Vec<String> = vec!["-nostdin".into(), "-y".into(), "-hide_banner".into()];
        for arg in argv {
            full.push(self.resolve_corpus_tokens(arg)?);
        }
        full.push(out.to_string_lossy().into_owned());
        let inv = Invocation::new(reference, full).with_timeout(Duration::from_secs(60));
        let obs = run(&inv).map_err(|e| format!("media `{}`: {e}", media.id))?;
        if !obs.succeeded() {
            return Err(format!(
                "media `{}`: the reference could not synthesise it.\n  {}\n{}",
                media.id,
                inv.command_line(),
                obs.stderr_text()
            ));
        }
        if !out.exists() {
            return Err(format!(
                "media `{}`: the reference exited 0 but wrote no file. That is \
                 usually a `generate` whose last argument is already an output \
                 path — the runner appends one.",
                media.id
            ));
        }
        self.built.borrow_mut().insert(key, out.clone());
        Ok(out)
    }

    /// Replace every `{corpus:<name>}` token in `arg` with the on-disk path
    /// [`MediaCache::corpus_path`] resolves `name` to.
    ///
    /// This is what lets a `generate` command remux a corpus bitstream
    /// through the reference (`ffmpeg -i {corpus:jvt-h264-canl1-sva-b} -c
    /// copy -f mp4 …`) instead of only ever synthesising media from
    /// scratch. It exists because JVT/JCT-VC conformance bitstreams are raw
    /// Annex-B elementary streams with no container-level timestamps, and
    /// this project's transcode pipeline currently requires every packet
    /// that reaches its filtering stage to carry one (see the case
    /// authored against `h264_decode`/`hevc_decode` for the measured error
    /// and why the fix belongs in a demux crate, not here). Wrapping the
    /// *same bytes* in an MP4 via the reference's own `-c copy` (which
    /// changes no NAL unit, only adds container timing) sidesteps that gap
    /// without weakening what is actually under test: byte-for-byte NAL
    /// data, still decoded by the codec under test either way — confirmed
    /// by hand on `jvt-h264-canl1-sva-b`, whose direct-elementary-stream
    /// reference decode and MP4-wrapped `vaco` decode are byte-identical.
    ///
    /// # Errors
    /// As [`MediaCache::corpus_path`], plus an unterminated `{corpus:` token.
    fn resolve_corpus_tokens(&self, arg: &str) -> Result<String, String> {
        let mut out = arg.to_owned();
        while let Some(start) = out.find("{corpus:") {
            let end = out
                .get(start..)
                .and_then(|rest| rest.find('}'))
                .map(|i| start + i)
                .ok_or_else(|| format!("unterminated `{{corpus:` in {out:?}"))?;
            let name = out
                .get(start + "{corpus:".len()..end)
                .ok_or_else(|| format!("malformed `{{corpus:...}}` token in {out:?}"))?
                .to_owned();
            let path = self.corpus_path(&name)?;
            out.replace_range(start..=end, &path.to_string_lossy());
        }
        Ok(out)
    }

    /// Materialise a `corpus://<name>` media reference: look `name` up in
    /// `vaco-corpus`'s own `vaco-media.lock` (embedded at compile time, same
    /// as [`crate::suites::resolve`] already joins against), fetch it
    /// through the shared content-addressed [`vaco_corpus::Store`] — a cache
    /// hit never touches the network; a miss does only when
    /// `VACO_CORPUS_NETWORK=1` (`vaco_corpus::NetworkPolicy::from_env`) —
    /// and, when the entry names an archive `member` (every JVT/JCT-VC
    /// conformance ZIP does), extract just that file rather than handing a
    /// case a whole ZIP to open.
    ///
    /// # Errors
    /// A message naming the corpus entry and what went wrong: not found in
    /// the lock, not fetchable offline, a hash mismatch, or (for an archive
    /// entry) a ZIP/member problem.
    fn corpus_path(&self, name: &str) -> Result<PathBuf, String> {
        let key = format!("corpus\u{1}{name}");
        if let Some(hit) = self.built.borrow().get(&key) {
            return Ok(hit.clone());
        }
        let lock = vaco_corpus::embedded_catalogue();
        let entry = lock
            .find(name)
            .ok_or_else(|| format!("corpus entry `{name}` is not in vaco-corpus's vaco-media.lock"))?;
        let store = vaco_corpus::Store::open_default();
        let policy = vaco_corpus::NetworkPolicy::from_env();
        let bytes =
            vaco_corpus::fetch::fetch_asset(entry, &store, policy).map_err(|e| format!("corpus `{name}`: {e}"))?;

        let dir = self
            .dir
            .as_ref()
            .ok_or("the media cache has no directory")?;
        let sub = dir.path().join(format!("m{}", self.built.borrow().len()));
        std::fs::create_dir_all(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
        // Keep whatever extension the archive member (or, for a bare-file
        // entry, the entry's own name) carries — format detection by
        // extension is part of what a transcode case exercises, the same
        // reasoning `MediaRef::file_name` already applies to `generate`d
        // media.
        let file_name = entry
            .member
            .as_deref()
            .and_then(|m| m.rsplit('/').next())
            .filter(|f| !f.is_empty())
            .unwrap_or(name);
        let out = sub.join(file_name);
        std::fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;

        self.built.borrow_mut().insert(key, out.clone());
        Ok(out)
    }
}

/// Where our own binaries live.
#[derive(Debug, Clone, Default)]
pub struct UnderTest {
    /// Our ffprobe equivalent.
    pub probe: Option<PathBuf>,
    /// Our ffmpeg equivalent.
    pub transcode: Option<PathBuf>,
    /// Our ffplay equivalent.
    pub play: Option<PathBuf>,
}

impl UnderTest {
    /// Locate our binaries: `VACO_BIN_*` first, then the target directory.
    #[must_use]
    pub fn discover() -> Self {
        Self {
            probe: find("VACO_BIN_PROBE", "vaco-probe"),
            transcode: find("VACO_BIN_VACO", "vaco"),
            play: find("VACO_BIN_PLAY", "vaco-play"),
        }
    }

    /// The binary for `tool`, if it is built.
    #[must_use]
    pub fn binary(&self, tool: Tool) -> Option<&PathBuf> {
        match tool {
            Tool::Probe => self.probe.as_ref(),
            Tool::Transcode => self.transcode.as_ref(),
            Tool::PlayHeadless => self.play.as_ref(),
            // `filter` never runs a subprocess for "ours" — see
            // `Runner::run_filter_case`, which returns before this ever
            // gets called for a filter case. `None` here would read as
            // "not built" if it were ever reached by mistake, which is the
            // right failure mode for a bug, not a silent pass.
            Tool::Filter => None,
        }
    }
}

fn find(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_key) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    ["debug", "release"]
        .iter()
        .map(|profile| root.join("target").join(profile).join(name))
        .find(|c| c.is_file())
}

/// One case's outcome, with everything needed to explain it.
#[derive(Debug)]
pub struct Outcome {
    /// Which case.
    pub case: Case,
    /// How it went.
    pub verdict: Verdict,
    /// The command we ran, for the failure report.
    pub ours_command: String,
    /// The command the reference ran.
    pub theirs_command: String,
}

/// Aggregate counts for a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Cases that agreed.
    pub agreed: u64,
    /// Cases whose every difference was allowlisted.
    pub allowed: u64,
    /// Cases with an unexplained difference.
    pub diverged: u64,
    /// Cases where one side failed to produce a comparable result.
    pub failed: u64,
    /// Cases that did not run.
    pub skipped: u64,
}

impl Tally {
    /// Total cases considered.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.agreed + self.allowed + self.diverged + self.failed + self.skipped
    }

    /// Fraction of cases that did not run.
    #[must_use]
    pub fn skip_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.skipped as f64 / total as f64
    }

    /// Whether the run should fail a gate.
    #[must_use]
    pub const fn is_failing(&self) -> bool {
        self.diverged > 0 || self.failed > 0
    }

    /// Fold one verdict into the tally. Public so a caller driving
    /// [`Runner::run_case`] directly — bypassing [`Runner::run_all`]'s tier
    /// filter, e.g. to reproduce one named case regardless of its declared
    /// tier — can still build an accurate [`Tally`] for the report.
    pub fn record(&mut self, verdict: &Verdict) {
        match verdict {
            Verdict::Agree => self.agreed += 1,
            Verdict::AllowedDivergence(_) => self.allowed += 1,
            Verdict::Divergence(_) => self.diverged += 1,
            Verdict::OursFailed(_) | Verdict::ReferenceFailed(_) => self.failed += 1,
            Verdict::Skipped(_) => self.skipped += 1,
        }
    }
}

/// Runs cases against a reference.
#[derive(Debug)]
pub struct Runner<'a> {
    /// The reference installation, if there is one.
    pub reference: Option<&'a Reference>,
    /// Why there isn't one, if there isn't.
    pub absent_reason: String,
    /// Our binaries.
    pub under_test: UnderTest,
    /// The divergence register.
    pub allowlist: &'a Allowlist,
    /// Media synthesised by the reference, shared across every case in the run.
    pub media: MediaCache,
}

impl<'a> Runner<'a> {
    /// A runner over `reference`.
    #[must_use]
    pub fn new(reference: Option<&'a Reference>, allowlist: &'a Allowlist) -> Self {
        Self {
            reference,
            absent_reason: String::new(),
            under_test: UnderTest::discover(),
            allowlist,
            media: MediaCache {
                // A failure to make the directory is not fatal here: it becomes
                // a per-case error with a real message, which is far easier to
                // act on than a constructor that cannot fail.
                dir: tempfile::tempdir().ok().map(std::sync::Arc::new),
                built: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            },
        }
    }

    /// Run a set of cases at `tier`, returning outcomes and a tally.
    #[must_use]
    pub fn run_all(&self, cases: &[Case], tier: Tier) -> (Vec<Outcome>, Tally) {
        let mut outcomes = Vec::new();
        let mut tally = Tally::default();
        for case in cases {
            if !case.tier.included_by(tier) {
                continue;
            }
            let outcome = self.run_case(case);
            tally.record(&outcome.verdict);
            outcomes.push(outcome);
        }
        (outcomes, tally)
    }

    /// Replace `{media}` and `{media:<id>}` placeholders in `argv`.
    fn substitute_media(&self, case: &Case, argv: &mut [String]) -> Result<(), String> {
        if !argv.iter().any(|a| a.contains("{media")) {
            return Ok(());
        }
        // A reference installation is required even for `corpus://` media,
        // which needs no synthesis: `run_case` always calls this before
        // running the reference side of the comparison, so by the time any
        // case reaches here a reference is assumed to exist regardless of
        // where its media comes from.
        let reference = self
            .reference
            .ok_or("a case references media but there is no reference installation")?;
        for arg in argv.iter_mut() {
            while let Some(start) = arg.find("{media") {
                let end = arg
                    .get(start..)
                    .and_then(|r| r.find('}'))
                    .map(|i| start + i)
                    .ok_or_else(|| format!("unterminated `{{media` in {arg:?}"))?;
                let token = arg.get(start + 1..end).unwrap_or_default();
                let wanted = token.strip_prefix("media:").map(str::trim);
                let media = match wanted {
                    None => case.media.first().ok_or_else(|| {
                        "`{media}` used but the suite declares no media".to_owned()
                    })?,
                    Some(id) => case
                        .media
                        .iter()
                        .find(|m| m.id == id)
                        .ok_or_else(|| format!("`{{media:{id}}}` names no declared media"))?,
                };
                let path = self.media.path(media, &reference.ffmpeg)?;
                arg.replace_range(start..=end, &path.to_string_lossy());
            }
        }
        Ok(())
    }

    /// Run one case.
    #[must_use]
    pub fn run_case(&self, case: &Case) -> Outcome {
        let skip = |reason: SkipReason| Outcome {
            case: case.clone(),
            verdict: Verdict::Skipped(reason),
            ours_command: String::new(),
            theirs_command: String::new(),
        };

        let Some(reference) = self.reference else {
            return skip(SkipReason::NoReference(if self.absent_reason.is_empty() {
                "no reference installation".to_owned()
            } else {
                self.absent_reason.clone()
            }));
        };

        // `filter` has no subprocess on "our" side at all (no `vaco -vf`
        // CLI exists yet), so it cannot go through the binary-discovery
        // check below, which exists to skip when a *subprocess* binary is
        // missing — a filter case is never in that position, and forcing
        // it through that check would either wrongly skip it or need a
        // fake binary path. See `Runner::run_filter_case`'s own doc.
        if case.tool == Tool::Filter {
            return self.run_filter_case(case, reference);
        }

        let Some(ours_bin) = self.under_test.binary(case.tool) else {
            return skip(SkipReason::ToolNotBuilt(format!(
                "the `{}` binary under test is not built; set VACO_BIN_{} or \
                 `cargo build` it",
                case.tool,
                match case.tool {
                    Tool::Probe => "PROBE",
                    Tool::Transcode => "VACO",
                    Tool::PlayHeadless => "PLAY",
                    Tool::Filter => unreachable!("returned above"),
                }
            )));
        };
        let theirs_bin = match case.tool {
            Tool::Probe => &reference.ffprobe,
            Tool::Transcode | Tool::PlayHeadless => &reference.ffmpeg,
            Tool::Filter => unreachable!("returned above"),
        };

        let dir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: String::new(),
                    theirs_command: String::new(),
                };
            }
        };

        let prefix = case.normalise.argv_prefix(case.tool);
        let suffix = case.normalise.positional_suffix(case.tool);
        let mut case_argv = case.argv.clone();
        if !suffix.is_empty() {
            // Positional for the transcode tools (§ `Chain::positional_suffix`):
            // every transcode suite in this repository ends its own argv with
            // the output path, so the insertion point is "just before the last
            // element" rather than "the front of the command line".
            let insert_at = case_argv.len().saturating_sub(1);
            case_argv.splice(insert_at..insert_at, suffix);
        }
        let mut argv = prefix;
        argv.extend(case_argv);

        // Substitute `{media}` / `{media:<id>}` with real paths. A case that
        // names media it does not declare fails loudly here rather than being
        // handed a literal `{media}` to open, which the reference would report
        // as a missing file and both sides would "agree" on — a false pass.
        match self.substitute_media(case, &mut argv) {
            Ok(()) => {}
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e)),
                    ours_command: String::new(),
                    theirs_command: String::new(),
                };
            }
        }

        // `{output}` / `{output:<name>}` each resolve to a path **inside this
        // side's own subdirectory**, not the shared case directory: the two
        // binaries run the same argv, and if both wrote `out.mkv` into the
        // same directory the second run would silently overwrite the first
        // one's file before it was ever compared. Two subdirectories mean
        // both files survive to the comparison stage.
        let ours_out_dir = dir.path().join("ours-out");
        let theirs_out_dir = dir.path().join("theirs-out");
        let mut ours_argv = argv.clone();
        let mut theirs_argv = argv;
        let ours_output_path = substitute_output(&mut ours_argv, &ours_out_dir);
        let theirs_output_path = substitute_output(&mut theirs_argv, &theirs_out_dir);
        if ours_output_path.is_some()
            && let Err(e) = std::fs::create_dir_all(&ours_out_dir)
        {
            return Outcome {
                case: case.clone(),
                verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                ours_command: String::new(),
                theirs_command: String::new(),
            };
        }
        if theirs_output_path.is_some()
            && let Err(e) = std::fs::create_dir_all(&theirs_out_dir)
        {
            return Outcome {
                case: case.clone(),
                verdict: Verdict::ReferenceFailed(FailureKind::LaunchFailed(e.to_string())),
                ours_command: String::new(),
                theirs_command: String::new(),
            };
        }

        let ours_inv = Invocation::new(ours_bin, ours_argv)
            .in_dir(dir.path())
            .with_timeout(case.timeout);
        let theirs_inv = Invocation::new(theirs_bin, theirs_argv)
            .in_dir(dir.path())
            .with_timeout(case.timeout);

        let ours = match run(&ours_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };
        let theirs = match run(&theirs_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::ReferenceFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };

        // A `structured-diff` transcode case is not asking "did the two
        // programs print the same thing to stdout" — a remuxer prints a
        // progress summary, not the thing under test. It is asking "are the
        // two files these programs just wrote structurally the same
        // container", which means probing *those files*, not diffing stdout.
        // Only takes over once both sides have actually produced a file: a
        // transcode failure is still caught the ordinary way, by the
        // exit-code co-assertion inside `compare::evaluate` below.
        if matches!(case.compare, Compare::StructuredDiff { .. })
            && case.tool == Tool::Transcode
            && ours.succeeded()
            && theirs.succeeded()
            && let (Some(op), Some(tp)) =
                (ours_output_path.as_deref(), theirs_output_path.as_deref())
        {
            return self.probe_produced_files(case, reference, op, tp);
        }

        let ours_output_file = ours_output_path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok());
        let theirs_output_file = theirs_output_path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok());
        let mut pair = Pair::new(&ours, &theirs);
        pair.ours_output_file = ours_output_file.as_deref();
        pair.theirs_output_file = theirs_output_file.as_deref();

        Outcome {
            verdict: compare::evaluate(case, &pair, self.allowlist),
            case: case.clone(),
            ours_command: ours_inv.command_line(),
            theirs_command: theirs_inv.command_line(),
        }
    }

    /// Run a `filter`-tool case. The reference side is a real `ffmpeg`
    /// subprocess reading the generated raw media and writing raw video to
    /// stdout, the same shape every other tool's `theirs` invocation has.
    /// "Ours" is not a second subprocess — [`crate::filterexec::run`]
    /// builds a real `vaco_filter_core::Graph` in-process and returns an
    /// [`crate::run::Observation`] shaped the same way, so the rest of this
    /// function — media substitution, the shared `compare::evaluate` call,
    /// the `Outcome` it returns — is identical in shape to every other
    /// case, and a filter case's `Verdict` means the same thing a
    /// transcode case's does.
    fn run_filter_case(&self, case: &Case, reference: &Reference) -> Outcome {
        let outcome = |verdict: Verdict, ours_command: String, theirs_command: String| Outcome {
            case: case.clone(),
            verdict,
            ours_command,
            theirs_command,
        };

        let mut argv = case.argv.clone();
        if let Err(e) = self.substitute_media(case, &mut argv) {
            return outcome(
                Verdict::OursFailed(FailureKind::LaunchFailed(e)),
                String::new(),
                String::new(),
            );
        }

        let args = match crate::filterexec::FilterArgs::parse(&argv) {
            Ok(a) => a,
            Err(e) => {
                return outcome(
                    Verdict::OursFailed(FailureKind::LaunchFailed(e)),
                    String::new(),
                    String::new(),
                );
            }
        };

        let filter_expr = if args.filter_args.is_empty() {
            args.filter_name.to_owned()
        } else {
            format!("{}={}", args.filter_name, args.filter_args)
        };

        // Pad 0 first (this case's long-standing single fields), then any
        // extra pads in declaration order -- one `-i` per input, in the
        // same order `filterexec::run` connects its own source nodes, so
        // stream index `N` here is pad `N` there.
        let all_inputs: Vec<(&str, &str, u32, u32)> = std::iter::once((
            args.media_path,
            args.in_pixfmt,
            args.in_width,
            args.in_height,
        ))
        .chain(
            args.extra_inputs
                .iter()
                .map(|e| (e.media_path, e.pixfmt, e.width, e.height)),
        )
        .collect();

        let mut theirs_argv: Vec<String> = [
            "-nostdin",
            "-hide_banner",
            "-y",
            "-fflags",
            "+bitexact",
            "-flags",
            "+bitexact",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for &(path, pixfmt, width, height) in &all_inputs {
            theirs_argv.extend(
                ["-f", "rawvideo", "-pix_fmt", pixfmt, "-s"]
                    .into_iter()
                    .map(str::to_owned),
            );
            theirs_argv.push(format!("{width}x{height}"));
            theirs_argv.push("-i".to_owned());
            theirs_argv.push(path.to_owned());
        }
        // A single input keeps `-vf`, unchanged from before extra inputs
        // existed, so no already-passing single-input case's exact
        // invocation shifts. `-filter_complex` is only reached for a case
        // that actually names more than one input.
        if all_inputs.len() == 1 {
            theirs_argv.push("-vf".to_owned());
            theirs_argv.push(filter_expr);
        } else {
            let mut labels = String::new();
            for i in 0..all_inputs.len() {
                use std::fmt::Write as _;
                let _ = write!(labels, "[{i}:v]");
            }
            theirs_argv.push("-filter_complex".to_owned());
            theirs_argv.push(format!("{labels}{filter_expr}"));
        }
        theirs_argv.extend(
            ["-f", "rawvideo", "-pix_fmt"]
                .into_iter()
                .map(str::to_owned),
        );
        theirs_argv.push(args.out_pixfmt.to_owned());
        theirs_argv.push("-".to_owned());

        let theirs_inv = Invocation::new(&reference.ffmpeg, theirs_argv).with_timeout(case.timeout);
        let theirs_command = theirs_inv.command_line();

        let theirs = match run(&theirs_inv) {
            Ok(o) => o,
            Err(e) => {
                return outcome(
                    Verdict::ReferenceFailed(FailureKind::LaunchFailed(e.to_string())),
                    String::new(),
                    theirs_command,
                );
            }
        };

        let ours_command = format!(
            "<in-process> {}={}",
            args.filter_name, args.filter_args
        );
        let ours = match crate::filterexec::run(&args) {
            Ok(o) => o,
            Err(e) => {
                return outcome(
                    Verdict::OursFailed(FailureKind::LaunchFailed(e)),
                    ours_command,
                    theirs_command,
                );
            }
        };

        let pair = Pair::new(&ours, &theirs);
        outcome(
            compare::evaluate(case, &pair, self.allowlist),
            ours_command,
            theirs_command,
        )
    }

    /// Probe two already-written files — one from each side of a transcode
    /// case — and structurally diff the listings, reusing the tested C6
    /// machinery in [`crate::compare::structured`] rather than inventing a
    /// second one. `-show_format -show_streams` is what the brief for the
    /// remux matrix asks for: stream count, codecs, durations, timestamps,
    /// not byte layout.
    ///
    /// Skips (does not fail) when `vaco-probe` is not built — a case that
    /// wants this needs *two* binaries under test, and a missing one is
    /// coverage that erodes, not a divergence.
    fn probe_produced_files(
        &self,
        case: &Case,
        reference: &Reference,
        ours_file: &Path,
        theirs_file: &Path,
    ) -> Outcome {
        let probe_argv = |path: &Path| -> Vec<String> {
            [
                "-hide_banner",
                "-bitexact",
                "-of",
                "default",
                "-show_format",
                "-show_streams",
            ]
            .into_iter()
            .map(str::to_owned)
            .chain(std::iter::once(path.to_string_lossy().into_owned()))
            .collect()
        };
        let Some(probe_bin) = self.under_test.probe.as_ref() else {
            return Outcome {
                case: case.clone(),
                verdict: Verdict::Skipped(SkipReason::ToolNotBuilt(
                    "structural comparison of a transcode case's output needs \
                     `vaco-probe`; set VACO_BIN_PROBE or `cargo build` it"
                        .to_owned(),
                )),
                ours_command: String::new(),
                theirs_command: String::new(),
            };
        };
        let ours_inv = Invocation::new(probe_bin, probe_argv(ours_file)).with_timeout(case.timeout);
        let theirs_inv =
            Invocation::new(&reference.ffprobe, probe_argv(theirs_file)).with_timeout(case.timeout);
        let ours_probe = match run(&ours_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };
        let theirs_probe = match run(&theirs_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::ReferenceFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };
        Outcome {
            verdict: compare::evaluate(
                case,
                &Pair::new(&ours_probe, &theirs_probe),
                self.allowlist,
            ),
            case: case.clone(),
            ours_command: ours_inv.command_line(),
            theirs_command: theirs_inv.command_line(),
        }
    }
}

/// Replace a single `{output}` or `{output:<name>}` token with a path inside
/// `out_dir`, and return that path if one was found.
///
/// Mirrors [`Runner::substitute_media`]'s token grammar. `{output}` names
/// `out.bin`; `{output:<name>}` names `<name>` — a suite building a matrix of
/// output containers writes `{output:out.mkv}`, `{output:out.avi}`, and so on,
/// which is what lets the extension vary per axis value the way it would in a
/// hand-typed command line.
fn substitute_output(argv: &mut [String], out_dir: &Path) -> Option<PathBuf> {
    let mut resolved = None;
    for arg in argv.iter_mut() {
        while let Some(start) = arg.find("{output") {
            let Some(end) = arg
                .get(start..)
                .and_then(|r| r.find('}'))
                .map(|i| start + i)
            else {
                break;
            };
            let token = arg.get(start + 1..end).unwrap_or_default();
            let name = token.strip_prefix("output:").map_or("out.bin", str::trim);
            let name = if name.is_empty() { "out.bin" } else { name };
            let path = out_dir.join(name);
            resolved = Some(path.clone());
            arg.replace_range(start..=end, &path.to_string_lossy());
        }
    }
    resolved
}

/// The default per-case budget when a suite does not name one.
pub const DEFAULT_CASE_TIMEOUT: Duration = crate::run::DEFAULT_TIMEOUT;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Runner, Tally, UnderTest};
    use crate::case::{Capture, Case, CaseId, Compare, Tier, Tool, Verdict};
    use crate::divergence::Allowlist;
    use crate::normalise::Chain;
    use std::time::Duration;

    fn a_case() -> Case {
        Case {
            id: CaseId::new("s", "m", &[]),
            tool: Tool::Probe,
            media: Vec::new(),
            argv: vec!["-show_pixel_formats".to_owned()],
            compare: Compare::ExactBytes {
                captures: vec![Capture::Stdout],
            },
            normalise: Chain::default(),
            requires: Vec::new(),
            timeout: Duration::from_secs(5),
            tier: Tier::Smoke,
        }
    }

    #[test]
    fn no_reference_skips_rather_than_fails() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let runner = Runner::new(None, &allow);
        let outcome = runner.run_case(&a_case());
        assert!(matches!(outcome.verdict, Verdict::Skipped(_)));
        assert!(!outcome.verdict.is_failure(), "absence is not failure");
    }

    #[test]
    fn a_skip_message_tells_the_contributor_what_to_do() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let mut runner = Runner::new(None, &allow);
        runner.absent_reason = "install FFmpeg 8.1".to_owned();
        match runner.run_case(&a_case()).verdict {
            Verdict::Skipped(reason) => assert!(reason.to_string().contains("install FFmpeg")),
            other => panic!("expected a skip, got {}", other.label()),
        }
    }

    #[test]
    fn tier_filtering_excludes_higher_tiers() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let runner = Runner::new(None, &allow);
        let mut full = a_case();
        full.tier = Tier::Full;
        let (outcomes, tally) = runner.run_all(&[a_case(), full], Tier::Smoke);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(tally.total(), 1);
    }

    #[test]
    fn a_tally_knows_when_it_is_failing() {
        let mut t = Tally {
            agreed: 10,
            skipped: 2,
            ..Tally::default()
        };
        assert!(!t.is_failing());
        assert!((t.skip_rate() - 2.0 / 12.0).abs() < 1e-9);
        t.diverged = 1;
        assert!(t.is_failing());
    }

    #[test]
    fn binary_discovery_never_panics_when_nothing_is_built() {
        let u = UnderTest::discover();
        // Whatever the answer is, asking must be safe.
        let _ = u.binary(Tool::Probe);
        let _ = u.binary(Tool::Transcode);
        let _ = u.binary(Tool::PlayHeadless);
    }

    #[test]
    fn output_token_resolves_to_a_path_inside_the_given_directory() {
        let mut argv = vec![
            "-f".to_owned(),
            "matroska".to_owned(),
            "{output:out.mkv}".to_owned(),
        ];
        let dir = std::path::Path::new("/tmp/some-case-dir");
        let resolved = super::substitute_output(&mut argv, dir);
        assert_eq!(resolved, Some(dir.join("out.mkv")));
        assert_eq!(
            argv.get(2).map(String::as_str),
            Some(dir.join("out.mkv").to_string_lossy().as_ref())
        );
    }

    #[test]
    fn a_bare_output_token_defaults_to_out_bin() {
        let mut argv = vec!["{output}".to_owned()];
        let dir = std::path::Path::new("/tmp/d");
        let resolved = super::substitute_output(&mut argv, dir);
        assert_eq!(resolved, Some(dir.join("out.bin")));
    }

    #[test]
    fn no_output_token_resolves_to_nothing() {
        let mut argv = vec!["-c".to_owned(), "copy".to_owned()];
        let resolved = super::substitute_output(&mut argv, std::path::Path::new("/tmp/d"));
        assert_eq!(resolved, None);
        assert_eq!(argv, vec!["-c".to_owned(), "copy".to_owned()]);
    }
}
