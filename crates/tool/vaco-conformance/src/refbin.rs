//! Reference-binary pinning and discovery (QA-03, plan 13 §1.6).
//!
//! # What it is
//!
//! The one place that answers "which `ffmpeg` am I comparing against, and is it
//! the one CI compares against?". Everything else in the harness takes a
//! [`Reference`] and never touches `PATH` itself.
//!
//! # How it works
//!
//! [`RefSpec`] is loaded from `refspec.toml`, which is compiled into the binary
//! with `include_str!` so the pin travels with the harness and cannot go
//! missing. `VACO_REFSPEC` overrides the path for anyone testing a bump.
//!
//! [`discover`] locates the binaries (`VACO_REF_FFMPEG` / `VACO_REF_FFPROBE`,
//! else `PATH`), runs `-version`, and classifies the result against the pins:
//!
//! | Outcome | Meaning | Effect |
//! |---|---|---|
//! | [`Channel::Stable`] | matches the gating pin | gating suites run and block |
//! | [`Channel::Next`] | matches the advisory pin | suites run, never block |
//! | [`Channel::Previous`] | matches the previous-major pin | §1.6.2 multi-version assertion only |
//! | [`Channel::Unpinned`] | some other version | advisory unless `VACO_CONFORMANCE_STRICT=1` |
//!
//! Absence is not failure. [`discover`] returns [`Discovery::Absent`] with a
//! sentence a contributor can act on, and every test in the harness turns that
//! into a skip. Plan 13 §1.5.4: a contributor without ffmpeg still runs
//! `cargo test`.
//!
//! # How to change it
//!
//! Bumping a pin is `refspec.toml` plus the drift triage in §1.6.2 — never a
//! code change here. Add a *channel* only if a new gating role genuinely
//! appears; each one costs a row in every report.
//!
//! # Configuration
//!
//! `VACO_REFSPEC`, `VACO_REF_FFMPEG`, `VACO_REF_FFPROBE`,
//! `VACO_CONFORMANCE_STRICT`.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::run::{self, Invocation};
use crate::toml::{self, Table};

/// The pin file, compiled in so the harness always has one.
const EMBEDDED_REFSPEC: &str = include_str!("../refspec.toml");

/// Which pin a discovered binary matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    /// The gating pin. Failures block.
    Stable,
    /// The advisory pin. Failures are reported, never blocking.
    Next,
    /// The previous major, for the multi-version assertion.
    Previous,
    /// Anything else. Advisory unless strict mode is on.
    Unpinned,
}

impl Channel {
    /// Whether a failure observed on this channel may block a merge.
    #[must_use]
    pub const fn gates(self) -> bool {
        matches!(self, Self::Stable)
    }

    /// The name used in reports and on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::Unpinned => "unpinned",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One pinned reference build.
#[derive(Debug, Clone)]
pub struct Pin {
    /// Which role this pin plays.
    pub channel: Channel,
    /// The released tag, e.g. `8.1`. Never a git snapshot (§1.6.1).
    pub version: String,
    /// Where the source tarball is fetched from.
    pub tarball: String,
    /// SHA-256 of the tarball. Empty means "not yet recorded"; the harness
    /// reports that rather than implying the pin is verified.
    pub sha256: String,
    /// Registry digest of the built image. CI pulls by digest, never by tag.
    pub image_digest: String,
    /// Whether failures on this channel block.
    pub gates: bool,
    /// The pinned configure line, which makes the build a function of the pin
    /// rather than of whatever happened to be on the builder.
    pub configure: Vec<String>,
}

/// A behaviour difference between two pins that has already been triaged.
#[derive(Debug, Clone)]
pub struct KnownDrift {
    /// Older version.
    pub from: String,
    /// Newer version.
    pub to: String,
    /// What differs, in one phrase.
    pub subject: String,
    /// One of `follow`, `regression`, `intentional-change`, `harness-artifact`.
    pub bucket: String,
    /// The triage note.
    pub note: String,
}

/// The whole pin file.
#[derive(Debug, Clone)]
pub struct RefSpec {
    /// Pins by channel.
    pub pins: BTreeMap<Channel, Pin>,
    /// Triaged behaviour differences between pins.
    pub drift: Vec<KnownDrift>,
}

impl RefSpec {
    /// Load the embedded pin file, or the one `VACO_REFSPEC` names.
    ///
    /// # Errors
    /// A parse or schema failure in the pin file.
    pub fn load() -> Result<Self, String> {
        match env::var_os("VACO_REFSPEC") {
            Some(path) => {
                let path = PathBuf::from(path);
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                Self::parse(&text)
            }
            None => Self::parse(EMBEDDED_REFSPEC),
        }
    }

    /// Parse a pin file.
    ///
    /// # Errors
    /// A parse or schema failure.
    pub fn parse(text: &str) -> Result<Self, String> {
        let doc = toml::parse(text).map_err(|e| format!("refspec: {e}"))?;
        if doc.get("schema").and_then(toml::Value::as_int) != Some(1) {
            return Err("refspec: `schema` must be 1".to_owned());
        }
        let mut pins = BTreeMap::new();
        for (key, channel) in [
            ("stable", Channel::Stable),
            ("next", Channel::Next),
            ("previous", Channel::Previous),
        ] {
            let Some(t) = doc.get(key).and_then(toml::Value::as_table) else {
                continue;
            };
            pins.insert(channel, pin_from(channel, t)?);
        }
        if !pins.contains_key(&Channel::Stable) {
            return Err("refspec: a [stable] pin is mandatory — it is what gates".to_owned());
        }
        let drift = doc
            .get("known_drift")
            .and_then(toml::Value::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(toml::Value::as_table)
            .map(|t| KnownDrift {
                from: string_or_empty(t, "from"),
                to: string_or_empty(t, "to"),
                subject: string_or_empty(t, "subject"),
                bucket: string_or_empty(t, "bucket"),
                note: string_or_empty(t, "note"),
            })
            .collect();
        Ok(Self { pins, drift })
    }

    /// The gating pin.
    ///
    /// # Panics
    /// Never: [`Self::parse`] rejects a spec without one.
    #[must_use]
    pub fn stable(&self) -> &Pin {
        self.pins.get(&Channel::Stable).unwrap_or_else(|| {
            // Unreachable: `parse` refuses a spec with no stable pin. Written
            // as a fallback rather than an `expect` because `expect_used` is
            // denied workspace-wide and a harness should not panic anyway.
            static FALLBACK: std::sync::OnceLock<Pin> = std::sync::OnceLock::new();
            FALLBACK.get_or_init(|| Pin {
                channel: Channel::Stable,
                version: String::new(),
                tarball: String::new(),
                sha256: String::new(),
                image_digest: String::new(),
                gates: true,
                configure: Vec::new(),
            })
        })
    }

    /// Classify a version string against the pins.
    #[must_use]
    pub fn classify(&self, version: &str) -> Channel {
        for (&channel, pin) in &self.pins {
            if pin.version == version {
                return channel;
            }
        }
        Channel::Unpinned
    }

    /// Drift entries that mention `version` on either side.
    #[must_use]
    pub fn drift_touching(&self, version: &str) -> Vec<&KnownDrift> {
        self.drift
            .iter()
            .filter(|d| d.from == version || d.to == version)
            .collect()
    }
}

fn pin_from(channel: Channel, t: &Table) -> Result<Pin, String> {
    let version = t
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("refspec: [{channel}] needs a `version`"))?
        .to_owned();
    if version.contains("git") || version.len() > 16 {
        return Err(format!(
            "refspec: [{channel}] version `{version}` is not a released tag; \
             §1.6.1 forbids snapshots because they are unreproducible oracles"
        ));
    }
    Ok(Pin {
        channel,
        version,
        tarball: string_or_empty(t, "tarball"),
        sha256: string_or_empty(t, "sha256"),
        image_digest: string_or_empty(t, "image_digest"),
        gates: t
            .get("gates")
            .and_then(toml::Value::as_bool)
            .unwrap_or(channel == Channel::Stable),
        configure: t
            .get("configure")
            .and_then(toml::Value::as_str_array)
            .unwrap_or_default(),
    })
}

fn string_or_empty(t: &Table, key: &str) -> String {
    t.get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A located, version-identified reference installation.
#[derive(Debug, Clone)]
pub struct Reference {
    /// Path to the transcoder binary.
    pub ffmpeg: PathBuf,
    /// Path to the prober binary.
    pub ffprobe: PathBuf,
    /// Normalised version, e.g. `8.1`.
    pub version: String,
    /// The raw first line of `-version`, kept for the run report.
    pub banner: String,
    /// Which pin it matched.
    pub channel: Channel,
}

impl Reference {
    /// Whether results from this installation may block a merge.
    #[must_use]
    pub fn gates(&self) -> bool {
        self.channel.gates()
    }
}

/// The result of looking for a reference installation.
#[derive(Debug, Clone)]
pub enum Discovery {
    /// Found, with the pin it matched.
    Found(Box<Reference>),
    /// Not found. The string is a sentence for a human.
    Absent(String),
}

impl Discovery {
    /// The reference, if there is one.
    #[must_use]
    pub fn reference(&self) -> Option<&Reference> {
        match self {
            Self::Found(r) => Some(r),
            Self::Absent(_) => None,
        }
    }

    /// The skip message, if there is no reference.
    #[must_use]
    pub fn skip_reason(&self) -> Option<&str> {
        match self {
            Self::Found(_) => None,
            Self::Absent(why) => Some(why),
        }
    }
}

/// Locate a reference installation and classify it against `spec`.
///
/// Never fails: absence is [`Discovery::Absent`], which callers turn into a
/// skip.
#[must_use]
pub fn discover(spec: &RefSpec) -> Discovery {
    let Some(ffmpeg) = locate("VACO_REF_FFMPEG", "ffmpeg") else {
        return Discovery::Absent(absent_message("ffmpeg", spec));
    };
    let Some(ffprobe) = locate("VACO_REF_FFPROBE", "ffprobe") else {
        return Discovery::Absent(absent_message("ffprobe", spec));
    };
    let banner = match version_banner(&ffmpeg) {
        Ok(b) => b,
        Err(e) => {
            return Discovery::Absent(format!(
                "found {} but could not run it ({e}); conformance checks skipped",
                ffmpeg.display()
            ));
        }
    };
    let version = normalise_version(&banner);
    let channel = spec.classify(&version);
    Discovery::Found(Box::new(Reference {
        ffmpeg,
        ffprobe,
        version,
        banner,
        channel,
    }))
}

/// Whether an unpinned reference should be treated as a hard error.
#[must_use]
pub fn strict() -> bool {
    env::var("VACO_CONFORMANCE_STRICT").is_ok_and(|v| v != "0" && !v.is_empty())
}

fn absent_message(what: &str, spec: &RefSpec) -> String {
    format!(
        "reference `{what}` not found on PATH; conformance checks skipped. \
         Install FFmpeg {} (the pinned reference), or set VACO_REF_FFMPEG / \
         VACO_REF_FFPROBE to a build of it. `cargo test` passes without it.",
        spec.stable().version
    )
}

fn locate(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = env::var_os(env_key) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|c| c.is_file())
}

fn version_banner(bin: &Path) -> Result<String, String> {
    let obs = run::run(&Invocation::new(bin, ["-version"])).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&obs.stdout);
    text.lines()
        .next()
        .map(str::trim)
        .map(str::to_owned)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| "-version printed nothing".to_owned())
}

/// Reduce `ffmpeg version n8.1-static Copyright …` to `8.1`.
///
/// Distribution builds decorate the version with a leading `n`, a trailing
/// `-static`, a Homebrew revision, or a git hash. The pin compares the plain
/// released tag, so everything decorative is stripped. A version that survives
/// stripping as something like `N-119384-g1a2b3c` will simply not match any
/// pin, which is the correct outcome: a snapshot is not a valid oracle.
#[must_use]
pub fn normalise_version(banner: &str) -> String {
    let Some(rest) = banner.split(" version ").nth(1) else {
        return String::new();
    };
    let token = rest.split_whitespace().next().unwrap_or_default();
    let token = token.strip_prefix('n').unwrap_or(token);
    let end = token
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(token.len());
    token
        .get(..end)
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_owned()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a None in a test is a failing test, which is the correct outcome"
)]
mod tests {
    use super::{Channel, RefSpec, normalise_version};

    #[test]
    fn version_normalisation_strips_distribution_decoration() {
        assert_eq!(
            normalise_version("ffmpeg version 8.1 Copyright (c) 2000-2026"),
            "8.1"
        );
        assert_eq!(
            normalise_version("ffmpeg version n8.1-static https://…"),
            "8.1"
        );
        assert_eq!(
            normalise_version("ffprobe version 7.1.1-tessus Copyright"),
            "7.1.1"
        );
        // A git snapshot must not accidentally normalise to a released tag.
        assert_eq!(normalise_version("ffmpeg version N-119384-g1a2b3c"), "");
        assert_eq!(normalise_version("no version here"), "");
    }

    #[test]
    fn embedded_spec_loads_and_pins_a_release() {
        let spec = RefSpec::parse(super::EMBEDDED_REFSPEC).expect("embedded refspec parses");
        let stable = spec.stable();
        assert!(stable.gates, "the stable pin must gate");
        assert!(!stable.version.contains("git"));
        assert_eq!(spec.classify(&stable.version), Channel::Stable);
        assert_eq!(spec.classify("0.0"), Channel::Unpinned);
        assert!(
            spec.pins.contains_key(&Channel::Next),
            "an advisory `next` pin is what makes drift visible early (§1.6.2)"
        );
    }

    #[test]
    fn snapshot_pins_are_rejected() {
        let text = "schema = 1\n[stable]\nversion = \"8.0.git\"\n";
        assert!(RefSpec::parse(text).is_err());
    }

    #[test]
    fn a_spec_without_stable_is_rejected() {
        let text = "schema = 1\n[next]\nversion = \"8.2\"\n";
        assert!(RefSpec::parse(text).is_err());
    }

    #[test]
    fn drift_is_indexed_by_version() {
        let spec = RefSpec::parse(super::EMBEDDED_REFSPEC).expect("parses");
        assert!(
            !spec.drift_touching("8.0").is_empty(),
            "the 8.0 -> 8.1 deltas are recorded so contributors see an \
             explanation instead of a mystery"
        );
    }
}
