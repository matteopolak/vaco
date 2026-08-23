//! Normalisers (plan 13 §1.3).
//!
//! # What it is
//!
//! Named, individually enabled transformations applied to an argument vector
//! before a run or to captured output after one. There is **no implicit
//! normalisation**: a case that wants any names it in its manifest, so a
//! reviewer sees the permitted blindness at a glance.
//!
//! # The review question
//!
//! For every proposed normaliser: *"name a bug this would conceal."* If the
//! answer is non-empty it is not a normaliser, it is a divergence-allowlist
//! entry with a scope, and that carries much heavier governance. Each variant
//! below carries its answer in its doc comment.
//!
//! # How to change it
//!
//! Add a variant, a name in [`Invocation::parse`] or [`Output::parse`], its
//! behaviour, and a unit test. Prefer an *invocation* normaliser to an *output*
//! one whenever both would work — deleting a difference is worse than never
//! creating it.

use std::collections::BTreeMap;

/// Applied to the argument vector, on both sides identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invocation {
    /// Adds `-bitexact` and the bitexact flags. Suppresses version-dependent
    /// output at source.
    ///
    /// *Conceals:* nothing — it changes what both programs are asked to do,
    /// identically, and the non-bitexact behaviour is covered by its own suite.
    ///
    /// **Positional for the transcode tools.** `-fflags`/`-flags` are
    /// *per-file* options: placed before every `-i` (which is where a naive
    /// prefix would put them) they configure the *input*, not the output, and
    /// a Matroska mux keeps writing a random Segment UID on every run.
    /// OBSERVED (`ffmpeg` 8.1, two runs of the identical command line, byte
    /// diff of the result): flags placed at the front of the command line
    /// give two *different* files on successive runs of the *same* binary;
    /// the same flags placed anywhere after the last `-i` and before the
    /// output path give byte-identical files every time. [`Chain`] therefore
    /// keeps this normaliser out of [`Chain::argv_prefix`] for anything but
    /// `probe` and supplies it positionally through
    /// [`Chain::positional_suffix`] instead — see that method.
    ///
    /// OBSERVED (`ffmpeg` 8.1): a `vaco` build as of this writing does not
    /// parse a bare `-flags` option at all (`Unrecognized option 'flags'.
    /// Error splitting the argument list`, exit 8) — every transcode case
    /// declaring plain `bitexact` fails to launch until that CLI gap closes.
    /// Recorded in `planning/CONFORMANCE-FINDINGS.md`, not silently patched
    /// around here: this normaliser keeps emitting both flags because a case
    /// that *encodes* (not just copies) needs `-flags +bitexact` too, and
    /// papering over a real gap in the tool under test by quietly weakening
    /// what the harness asks it to do is exactly the failure mode §1.4.2
    /// exists to prevent. [`Invocation::BitExactCopy`] is the narrower,
    /// already-usable alternative for `-c copy` cases.
    BitExact,
    /// Adds `-fflags +bitexact` alone — never `-flags` — positioned the same
    /// way as [`Invocation::BitExact`].
    ///
    /// *Conceals:* nothing, for a `-c copy` case specifically: `-flags`
    /// selects encoder/decoder-level bitexact behaviour, and a stream copy
    /// invokes neither. OBSERVED (`ffmpeg` 8.1): `-fflags +bitexact` alone is
    /// sufficient for two-run byte determinism on every `-c copy` remux in
    /// `tests/conformance/transcode/` — confirmed directly, not assumed,
    /// because "adequate for this narrower case" is exactly the kind of claim
    /// this crate does not get to make from a spec reading.
    ///
    /// Use [`Invocation::BitExact`] instead for any case that encodes.
    BitExactCopy,
    /// Adds `-hide_banner`, plus `-nostdin` for the tools that have it.
    ///
    /// *Conceals:* build-configuration text, which is meaningless to compare
    /// and which §1.4.2 `identification` covers where a case does test it.
    ///
    /// Plan 13 §1.3.1 says this adds `-hide_banner -nostdin` unconditionally.
    /// OBSERVED: `ffprobe` has no `-nostdin` and rejects the invocation with
    /// "Option not found", so it is added only for the transcode tools.
    HideBanner,
    /// Pins `-loglevel` on both sides.
    ///
    /// *Conceals:* nothing; stderr volume is otherwise environment-dependent.
    LogLevel,
    /// Copies the media into the case temp dir under a fixed name.
    ///
    /// *Conceals:* nothing; it makes `format.filename` compare equal without
    /// post-hoc string surgery, which would conceal filename-handling bugs.
    PathToken,
}

impl Invocation {
    /// Parse the manifest spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bitexact" => Some(Self::BitExact),
            "bitexact-copy" => Some(Self::BitExactCopy),
            "hide-banner" => Some(Self::HideBanner),
            "loglevel" => Some(Self::LogLevel),
            "path-token" => Some(Self::PathToken),
            _ => None,
        }
    }

    /// The manifest spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BitExact => "bitexact",
            Self::BitExactCopy => "bitexact-copy",
            Self::HideBanner => "hide-banner",
            Self::LogLevel => "loglevel",
            Self::PathToken => "path-token",
        }
    }

    /// Whether this normaliser's arguments must land after the last `-i` and
    /// before the output path for `tool`, rather than at the front of the
    /// command line. See [`Invocation::BitExact`]'s doc comment for the
    /// measurement behind this.
    #[must_use]
    pub const fn is_positional_for(self, tool: crate::case::Tool) -> bool {
        matches!(
            (self, tool),
            (
                Self::BitExact | Self::BitExactCopy,
                crate::case::Tool::Transcode | crate::case::Tool::PlayHeadless
            )
        )
    }

    /// Arguments this normaliser contributes, for `tool`. The caller decides
    /// where they go — see [`Invocation::is_positional_for`].
    #[must_use]
    pub fn prefix(self, tool: crate::case::Tool, loglevel: &str) -> Vec<String> {
        let own = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        match (self, tool) {
            (Self::BitExact | Self::BitExactCopy, crate::case::Tool::Probe) => own(&["-bitexact"]),
            (Self::BitExact, _) => own(&["-fflags", "+bitexact", "-flags", "+bitexact"]),
            (Self::BitExactCopy, _) => own(&["-fflags", "+bitexact"]),
            (Self::HideBanner, crate::case::Tool::Probe) => own(&["-hide_banner"]),
            (Self::HideBanner, _) => own(&["-hide_banner", "-nostdin"]),
            (Self::LogLevel, _) => vec!["-loglevel".to_owned(), loglevel.to_owned()],
            (Self::PathToken, _) => Vec::new(),
        }
    }
}

/// Applied to captured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Output {
    /// Removes `program_version` and `library_versions` sections entirely.
    ///
    /// *Conceals:* nothing that is ours to get right — these sections identify
    /// the producing software. Cases that *test* them use C6 with dedicated
    /// allowlist entries.
    StripSections,
    /// CRLF to LF.
    ///
    /// *Conceals:* nothing; it is an artifact of the harness's own capture on
    /// Windows, not of either program.
    LineEndings,
    /// Canonicalises `-0` versus `0` and the spelling of infinities.
    ///
    /// *Conceals:* real formatting bugs — which is why it is enabled **only**
    /// when the case declares a float tolerance, and never by default.
    FloatCanonical,
    /// Reduces stderr to a sorted multiset of severity levels.
    ///
    /// *Conceals:* message text, which is deliberately ours (§1.4.2
    /// `identification`). Severity is behaviour and is still compared.
    StderrClass,
    /// Restricts a listing to the intersection of both component sets.
    ///
    /// *Conceals:* a component we emit that the reference does not — which is
    /// why the caller additionally asserts the subset relation.
    ComponentIntersection,
}

impl Output {
    /// Parse the manifest spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "strip-sections" => Some(Self::StripSections),
            "line-endings" => Some(Self::LineEndings),
            "float-canonical" => Some(Self::FloatCanonical),
            "stderr-class" => Some(Self::StderrClass),
            "component-intersection" => Some(Self::ComponentIntersection),
            _ => None,
        }
    }

    /// The manifest spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StripSections => "strip-sections",
            Self::LineEndings => "line-endings",
            Self::FloatCanonical => "float-canonical",
            Self::StderrClass => "stderr-class",
            Self::ComponentIntersection => "component-intersection",
        }
    }

    /// Apply to one captured stream.
    #[must_use]
    pub fn apply(self, text: &str) -> String {
        match self {
            Self::StripSections => strip_sections(text),
            Self::LineEndings => text.replace("\r\n", "\n"),
            Self::FloatCanonical => float_canonical(text),
            Self::StderrClass => stderr_class(text),
            // Needs both sides at once; see `intersect`.
            Self::ComponentIntersection => text.to_owned(),
        }
    }
}

/// The named chain a case declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chain {
    /// Applied to argv, in declaration order.
    pub invocation: Vec<Invocation>,
    /// Applied to output, in declaration order.
    pub output: Vec<Output>,
    /// The level `loglevel` pins.
    pub loglevel: String,
}

impl Chain {
    /// Whether anything at all is applied. A case with a non-empty chain must
    /// declare mode `exact-bytes-normalised`, not `exact-bytes` (§1.2 C1).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invocation.is_empty() && self.output.is_empty()
    }

    /// Arguments to prepend for `tool`.
    ///
    /// Excludes anything [`Invocation::is_positional_for`] `tool` — those
    /// come from [`Chain::positional_suffix`] instead. Prepending
    /// `bitexact`/`bitexact-copy` here would put `-fflags`/`-flags` before
    /// `-i`, which sets them on the *input*; see [`Invocation::BitExact`]'s
    /// doc comment for the measurement.
    #[must_use]
    pub fn argv_prefix(&self, tool: crate::case::Tool) -> Vec<String> {
        let level = self.level();
        self.invocation
            .iter()
            .filter(|n| !n.is_positional_for(tool))
            .flat_map(|n| n.prefix(tool, level))
            .collect()
    }

    /// Arguments that must land **after the last input and before the output
    /// path**, for `tool` — every normaliser this chain declares for which
    /// [`Invocation::is_positional_for`] `tool` is true, in declaration
    /// order.
    ///
    /// The harness assembles a transcode case's argv as
    /// `[prefix] ++ [case argv]`, and every transcode-tool suite in this
    /// repository ends its own argv with the output path (the `{output}`
    /// token is always the final axis value) — so the caller inserts this
    /// immediately before the last element of the assembled argv.
    #[must_use]
    pub fn positional_suffix(&self, tool: crate::case::Tool) -> Vec<String> {
        let level = self.level();
        self.invocation
            .iter()
            .filter(|n| n.is_positional_for(tool))
            .flat_map(|n| n.prefix(tool, level))
            .collect()
    }

    fn level(&self) -> &str {
        if self.loglevel.is_empty() {
            "error"
        } else {
            &self.loglevel
        }
    }

    /// Apply the output chain.
    #[must_use]
    pub fn apply_output(&self, text: &str) -> String {
        self.output
            .iter()
            .fold(text.to_owned(), |acc, n| n.apply(&acc))
    }

    /// Parse a manifest `[normalise]` table.
    ///
    /// # Errors
    /// An unknown normaliser name. Silently ignoring one would mean a case
    /// runs without the blindness its author declared, which is worse than a
    /// hard failure.
    pub fn from_manifest(t: &crate::toml::Table) -> Result<Self, String> {
        use crate::toml::Value;
        let mut chain = Self {
            loglevel: t
                .get("loglevel")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_owned(),
            ..Self::default()
        };
        for name in t
            .get("invocation")
            .and_then(Value::as_str_array)
            .unwrap_or_default()
        {
            chain.invocation.push(
                Invocation::parse(&name)
                    .ok_or_else(|| format!("unknown invocation normaliser `{name}`"))?,
            );
        }
        for name in t
            .get("output")
            .and_then(Value::as_str_array)
            .unwrap_or_default()
        {
            chain.output.push(
                Output::parse(&name)
                    .ok_or_else(|| format!("unknown output normaliser `{name}`"))?,
            );
        }
        Ok(chain)
    }
}

/// Reduce both listings to the components they share, and report anything we
/// emit that the reference does not.
///
/// The second half is the point: `component-intersection` would otherwise
/// conceal us shipping a component the reference has never heard of, so the
/// subset assertion travels with the normaliser rather than being an optional
/// extra somewhere else.
#[must_use]
pub fn intersect<'a>(
    ours: &'a [String],
    theirs: &'a [String],
) -> (Vec<&'a String>, Vec<&'a String>) {
    let their_set: std::collections::BTreeSet<&str> = theirs.iter().map(String::as_str).collect();
    let shared = ours
        .iter()
        .filter(|o| their_set.contains(o.as_str()))
        .collect();
    let only_ours = ours
        .iter()
        .filter(|o| !their_set.contains(o.as_str()))
        .collect();
    (shared, only_ours)
}

fn strip_sections(text: &str) -> String {
    const DROP: [&str; 2] = ["PROGRAM_VERSION", "LIBRARY_VERSION"];
    let mut out = String::new();
    let mut depth = 0_usize;
    for line in text.lines() {
        let t = line.trim();
        let is_open = t
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .is_some_and(|n| !n.starts_with('/') && DROP.iter().any(|d| n.starts_with(d)));
        let is_close = t
            .strip_prefix("[/")
            .and_then(|r| r.strip_suffix(']'))
            .is_some_and(|n| DROP.iter().any(|d| n.starts_with(d)));
        if is_open {
            depth += 1;
            continue;
        }
        if is_close {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 0 {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn float_canonical(text: &str) -> String {
    text.replace("-0.000000", "0.000000")
        .replace("-inf", "-Infinity")
        .replace("inf", "Infinity")
        .replace("-Infinity", "-inf")
        .replace("Infinity", "inf")
}

fn stderr_class(text: &str) -> String {
    use std::fmt::Write as _;
    const LEVELS: [&str; 5] = ["fatal", "error", "warning", "info", "debug"];
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(level) = LEVELS.iter().find(|l| lower.contains(**l)) {
            *counts.entry(level).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .fold(String::new(), |mut acc, (level, n)| {
            let _ = writeln!(acc, "{level}={n}");
            acc
        })
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Chain, Invocation, Output, intersect};
    use crate::case::Tool;

    #[test]
    fn nostdin_is_only_added_where_it_exists() {
        // OBSERVED: `ffprobe -nostdin` fails with "Option not found".
        assert_eq!(
            Invocation::HideBanner.prefix(Tool::Probe, "error"),
            vec!["-hide_banner".to_owned()]
        );
        assert_eq!(
            Invocation::HideBanner.prefix(Tool::Transcode, "error"),
            vec!["-hide_banner", "-nostdin"]
        );
    }

    #[test]
    fn bitexact_differs_between_the_two_tools() {
        assert_eq!(
            Invocation::BitExact.prefix(Tool::Probe, "error"),
            vec!["-bitexact".to_owned()]
        );
        assert_eq!(
            Invocation::BitExact.prefix(Tool::Transcode, "error"),
            vec!["-fflags", "+bitexact", "-flags", "+bitexact"]
        );
    }

    #[test]
    fn strip_sections_removes_only_the_identifying_ones() {
        let input = "[PROGRAM_VERSION]\nversion=8.1\n[/PROGRAM_VERSION]\n\
                     [FORMAT]\nformat_name=mov\n[/FORMAT]\n";
        assert_eq!(
            Output::StripSections.apply(input),
            "[FORMAT]\nformat_name=mov\n[/FORMAT]\n"
        );
    }

    #[test]
    fn strip_sections_leaves_a_file_without_them_alone() {
        let input = "[FORMAT]\nformat_name=mov\n[/FORMAT]\n";
        assert_eq!(Output::StripSections.apply(input), input);
    }

    #[test]
    fn line_endings_are_platform_artifacts() {
        assert_eq!(Output::LineEndings.apply("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn stderr_class_keeps_severity_and_drops_prose() {
        let a = Output::StderrClass.apply("[error] our wording here\n[warning] and ours\n");
        let b = Output::StderrClass.apply("[error] their wording\n[warning] theirs\n");
        assert_eq!(a, b);
        assert_eq!(a, "error=1\nwarning=1\n");
    }

    #[test]
    fn stderr_class_still_catches_a_missing_diagnostic() {
        let a = Output::StderrClass.apply("[error] x\n");
        let b = Output::StderrClass.apply("");
        assert_ne!(a, b, "a lost error must still be a difference");
    }

    #[test]
    fn float_canonical_only_touches_negative_zero_and_infinities() {
        assert_eq!(Output::FloatCanonical.apply("-0.000000"), "0.000000");
        assert_eq!(Output::FloatCanonical.apply("1.500000"), "1.500000");
    }

    #[test]
    fn intersection_reports_what_only_we_have() {
        let ours = vec!["a".to_owned(), "b".to_owned(), "z".to_owned()];
        let theirs = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let (shared, only_ours) = intersect(&ours, &theirs);
        assert_eq!(shared.len(), 2);
        assert_eq!(only_ours, vec![&"z".to_owned()]);
    }

    #[test]
    fn an_unknown_normaliser_is_a_hard_error() {
        let doc = crate::toml::parse("[normalise]\noutput = [\"make-it-pass\"]\n").expect("parses");
        let t = doc
            .get("normalise")
            .and_then(crate::toml::Value::as_table)
            .expect("table");
        assert!(Chain::from_manifest(t).is_err());
    }

    #[test]
    fn bitexact_is_positional_for_transcode_but_not_for_probe() {
        let doc = crate::toml::parse("[normalise]\ninvocation = [\"bitexact\"]\n").expect("parses");
        let t = doc
            .get("normalise")
            .and_then(crate::toml::Value::as_table)
            .expect("table");
        let chain = Chain::from_manifest(t).expect("valid");

        // Probe: the flag is a single prefix argument, never positional.
        assert_eq!(chain.argv_prefix(Tool::Probe), vec!["-bitexact"]);
        assert!(chain.positional_suffix(Tool::Probe).is_empty());

        // Transcode: nothing is safe to prepend before `-i`; the flags come
        // back from `positional_suffix` instead.
        assert!(chain.argv_prefix(Tool::Transcode).is_empty());
        assert_eq!(
            chain.positional_suffix(Tool::Transcode),
            vec!["-fflags", "+bitexact", "-flags", "+bitexact"]
        );
    }

    #[test]
    fn bitexact_copy_omits_the_flags_option_vaco_does_not_parse() {
        let doc =
            crate::toml::parse("[normalise]\ninvocation = [\"bitexact-copy\"]\n").expect("parses");
        let t = doc
            .get("normalise")
            .and_then(crate::toml::Value::as_table)
            .expect("table");
        let chain = Chain::from_manifest(t).expect("valid");

        assert!(chain.argv_prefix(Tool::Transcode).is_empty());
        // Just `-fflags`, never `-flags`: OBSERVED (vaco, current build),
        // `-flags` is an unrecognised option and any case that sends it never
        // gets past argument parsing (exit 8).
        assert_eq!(
            chain.positional_suffix(Tool::Transcode),
            vec!["-fflags", "+bitexact"]
        );
    }

    #[test]
    fn a_chain_without_bitexact_has_no_positional_suffix() {
        let doc =
            crate::toml::parse("[normalise]\ninvocation = [\"hide-banner\"]\n").expect("parses");
        let t = doc
            .get("normalise")
            .and_then(crate::toml::Value::as_table)
            .expect("table");
        let chain = Chain::from_manifest(t).expect("valid");
        assert!(chain.positional_suffix(Tool::Transcode).is_empty());
    }

    #[test]
    fn a_declared_chain_round_trips() {
        let doc = crate::toml::parse(
            "[normalise]\ninvocation = [\"bitexact\", \"hide-banner\"]\noutput = [\"line-endings\"]\n",
        )
        .expect("parses");
        let t = doc
            .get("normalise")
            .and_then(crate::toml::Value::as_table)
            .expect("table");
        let chain = Chain::from_manifest(t).expect("valid");
        assert!(!chain.is_empty());
        let argv = chain.argv_prefix(Tool::Probe);
        assert_eq!(argv, vec!["-bitexact", "-hide_banner"]);
    }
}
