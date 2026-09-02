//! Suite manifests and matrix expansion (plan 13 §1.5.1).
//!
//! # What it is
//!
//! Cases are **data, not code**. A suite is one TOML file declaring its media,
//! its argument axes, and one `[compare]` block; the loader takes the cartesian
//! product of the axes, subtracts the declared exclusions, and yields one
//! [`Case`] per combination. A twenty-line file becomes thousands of cases,
//! which is the only way the case count in §1.8 is reachable — and a generator
//! written in code would not be reviewable.
//!
//! # How it works
//!
//! ```toml
//! schema = 1
//! suite  = "probe-isobmff"
//! tool   = "probe"
//! tier   = "core"
//! owner  = "@isobmff-owner"
//!
//! [[media]]
//! id       = "mpeg4-30f"
//! source   = "generated://mpeg4-30f.mp4"
//! tags     = ["video"]
//! generate = ["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:d=1.2",
//!             "-c:v", "mpeg4", "-f", "mp4"]
//!
//! [[axis]]
//! name   = "writer"
//! values = [
//!   { id = "json", argv = ["-of", "json"] },
//!   { id = "xml",  argv = ["-of", "xml"] },
//! ]
//!
//! [[exclude]]
//! when   = { writer = ["xml"] }
//! reason = "documented as incompatible with -sexagesimal"
//!
//! [compare]
//! mode    = "exact-bytes"
//! capture = ["stdout", "exit-code"]
//! timeout = "20s"
//!
//! [normalise]
//! invocation = ["bitexact", "hide-banner"]
//! output     = ["line-endings"]
//! ```
//!
//! `generate` synthesises the media with the reference binary at run time
//! rather than fetching it, and the runner appends the output path. That is
//! what makes the harness usable before the corpus machinery of QA-04/X-05
//! exists, and it stays inside D6: expected values are generated fresh at test
//! time and discarded, so nothing FFmpeg-derived enters the repository. A case
//! refers to its media as `{media}`, or `{media:<id>}` when a suite declares
//! several. An axis value that needs *more* than the one media its case
//! iterates to (a multi-input `filter`-tool case, most often) names the
//! rest with `extra_media = ["id", ...]`, resolved by id against the
//! suite's full `[[media]]` list rather than iterated — declaring three
//! extra inputs does not multiply the case count into three. A
//! `[[media]]` entry that exists only to be named this way, never as the
//! per-case iterated one, marks itself `fixed = true` so `Suite::expand`'s
//! iteration skips it. See [`AxisValue::extra_media`] and
//! [`MediaRef::fixed`](crate::case::MediaRef::fixed).
//!
//! Case ids come out as `suite/media/axis=value,axis=value`, in axis
//! declaration order, which is what makes them stable enough to paste into a
//! bug report (§1.5.2).
//!
//! # Two rules the loader enforces
//!
//! 1. **A non-empty normalisation chain requires mode `exact-bytes-normalised`,
//!    not `exact-bytes`.** §1.2 C1 keeps the two modes distinct precisely so a
//!    reviewer sees the blindness named in the manifest; a loader that let C0
//!    quietly carry normalisers would defeat that.
//! 2. **`structured-diff` requires a reason.** C6 is weaker than C0 by
//!    construction and §1.2 forbids using it to launder a failing C0 case, so a
//!    suite that picks it must say why in `downgrade_reason`.
//!
//! # How to change it
//!
//! New axes and new comparison parameters are manifest-only changes. Adding a
//! *structural* key means a field here, a check in [`Suite::parse`], and a
//! sentence in the crate doc — the manifest is a contract with case authors and
//! changing it silently breaks every suite at once.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::case::{Case, CaseId, Compare, MediaRef, Tier, Tool};
use crate::normalise::Chain;
use crate::toml::{self, Table, Value};

/// One value on one axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisValue {
    /// Appears in the case id.
    pub id: String,
    /// Arguments this value contributes.
    pub argv: Vec<String>,
    /// Optional per-value tier override.
    pub tier: Option<Tier>,
    /// `[[media]]` ids to attach to the case *in addition to* the one the
    /// suite's per-case media iteration already attaches, for a case that
    /// genuinely needs more than one input (a multi-input `filter`-tool
    /// case's second/third/... source). Resolved once at [`Suite::expand`]
    /// time, by id, against the suite's full declared `[[media]]` list --
    /// not iterated the way the base media is, so declaring `n` extra ids
    /// does not multiply the case count. Empty for every axis value that
    /// does not need it (every case today but a multi-input `filter` one).
    /// A case's argv then names each one explicitly via `{media:<id>}` --
    /// see `manifest`'s crate doc -- so which token is which input is a
    /// property of the suite file's own text, not of iteration order.
    pub extra_media: Vec<String>,
}

/// One axis of the matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    /// Appears in the case id.
    pub name: String,
    /// Its values, in declaration order.
    pub values: Vec<AxisValue>,
}

/// A combination the suite declares meaningless.
///
/// `when` maps an axis name to the values it excludes, exactly as `[[axis]]`
/// names them — with one synthetic exception: `media`, matched against the
/// id of the `[[media]]` entry a case was built from. There is no `[[axis]]`
/// named `media`; declaring several media entries is how a suite gets an
/// input-container dimension at all (`Suite::expand` loops over them the same
/// way it loops over an axis's values), and a remux matrix needs to say
/// things like `when = { media = ["avi"], output = ["matroska"] }` — measured
/// directly against the reference, not guessed, since AVI's H.264 packets
/// have no timestamps and Matroska's muxer refuses to write one that lacks
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// Axis name (or `media`, see above) to the values it excludes.
    pub when: BTreeMap<String, Vec<String>>,
    /// Why. Required — an unexplained exclusion is a hidden coverage hole.
    pub reason: String,
}

impl Exclusion {
    fn matches(&self, selection: &[(String, String)]) -> bool {
        self.when.iter().all(|(axis, values)| {
            selection
                .iter()
                .any(|(name, value)| name == axis && values.contains(value))
        })
    }
}

/// A whole suite file.
#[derive(Debug, Clone)]
pub struct Suite {
    /// Suite name; the first component of every case id.
    pub name: String,
    /// Which binary pair.
    pub tool: Tool,
    /// Default tier for cases in this suite.
    pub tier: Tier,
    /// Accountable owner.
    pub owner: String,
    /// Inputs.
    pub media: Vec<MediaRef>,
    /// Per-media tier overrides, by media id.
    pub media_tier: BTreeMap<String, Tier>,
    /// The matrix.
    pub axes: Vec<Axis>,
    /// Meaningless combinations.
    pub excludes: Vec<Exclusion>,
    /// How outputs are compared.
    pub compare: Compare,
    /// The declared normalisation chain.
    pub normalise: Chain,
    /// Features we must have for the suite to run.
    pub requires: Vec<String>,
    /// Per-case wall-clock budget.
    pub timeout: Duration,
}

impl Suite {
    /// Load and validate one suite file.
    ///
    /// # Errors
    /// A parse failure, a schema violation, or either of the two rules in the
    /// module docs.
    pub fn parse(text: &str) -> Result<Self, String> {
        let doc = toml::parse(text).map_err(|e| e.to_string())?;
        if doc.get("schema").and_then(Value::as_int) != Some(1) {
            return Err("`schema` must be 1".to_owned());
        }
        let name = string(&doc, "suite").ok_or("a suite needs a `suite` name")?;
        let tool = string(&doc, "tool")
            .and_then(|s| Tool::parse(&s))
            .ok_or("a suite needs a valid `tool`")?;
        let tier = string(&doc, "tier")
            .and_then(|s| Tier::parse(&s))
            .ok_or("a suite needs a valid `tier`")?;
        let owner = string(&doc, "owner").ok_or("a suite needs an `owner`")?;

        let mut media = Vec::new();
        let mut media_tier = BTreeMap::new();
        for raw in doc
            .get("media")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            let t = raw.as_table().ok_or("[[media]] must be a table")?;
            let id = string(t, "id").ok_or("a media entry needs an `id`")?;
            if let Some(tier) = string(t, "tier").and_then(|s| Tier::parse(&s)) {
                media_tier.insert(id.clone(), tier);
            }
            let generate = t.get("generate").and_then(Value::as_str_array);
            if let Some(g) = &generate
                && g.is_empty()
            {
                return Err(format!(
                    "media `{id}`: `generate` is present but empty — omit the key \
                     rather than declaring a synthesis that produces nothing"
                ));
            }
            media.push(MediaRef {
                source: string(t, "source").ok_or("a media entry needs a `source`")?,
                tags: t
                    .get("tags")
                    .and_then(Value::as_str_array)
                    .unwrap_or_default(),
                generate,
                fixed: t.get("fixed").and_then(Value::as_bool).unwrap_or(false),
                id,
            });
        }

        let mut axes = Vec::new();
        for raw in doc
            .get("axis")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            let t = raw.as_table().ok_or("[[axis]] must be a table")?;
            let axis_name = string(t, "name").ok_or("an axis needs a `name`")?;
            let mut values = Vec::new();
            for v in t
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("axis `{axis_name}` needs `values`"))?
            {
                let vt = v.as_table().ok_or("an axis value must be a table")?;
                values.push(AxisValue {
                    id: string(vt, "id").ok_or("an axis value needs an `id`")?,
                    argv: vt
                        .get("argv")
                        .and_then(Value::as_str_array)
                        .unwrap_or_default(),
                    tier: string(vt, "tier").and_then(|s| Tier::parse(&s)),
                    extra_media: vt
                        .get("extra_media")
                        .and_then(Value::as_str_array)
                        .unwrap_or_default(),
                });
            }
            if values.is_empty() {
                return Err(format!("axis `{axis_name}` has no values"));
            }
            axes.push(Axis {
                name: axis_name,
                values,
            });
        }

        let mut excludes = Vec::new();
        for raw in doc
            .get("exclude")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            let t = raw.as_table().ok_or("[[exclude]] must be a table")?;
            let reason = string(t, "reason").ok_or(
                "an exclusion needs a `reason`; an unexplained one is a hidden coverage hole",
            )?;
            let when_t = t
                .get("when")
                .and_then(Value::as_table)
                .ok_or("an exclusion needs a `when`")?;
            let mut when = BTreeMap::new();
            for (axis, v) in when_t {
                let values = match v {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(_) => v.as_str_array().ok_or("`when` values must be strings")?,
                    other => {
                        return Err(format!(
                            "`when.{axis}` must be a string or an array, not a {}",
                            other.kind()
                        ));
                    }
                };
                when.insert(axis.clone(), values);
            }
            excludes.push(Exclusion { when, reason });
        }

        let compare_t = doc
            .get("compare")
            .and_then(Value::as_table)
            .ok_or("a suite needs a [compare]")?;
        let compare = Compare::from_manifest(compare_t)?;
        let normalise = doc
            .get("normalise")
            .and_then(Value::as_table)
            .map_or_else(|| Ok(Chain::default()), Chain::from_manifest)?;

        // Rule 1, and only for **output** normalisers.
        //
        // The first version of this check counted the invocation chain too, and
        // that is a category error. An invocation normaliser adds the same flag
        // to both command lines — `-bitexact`, `-hide_banner` — which controls a
        // variable rather than hiding a difference: whatever the two binaries
        // then print, the comparison still sees every byte of it. An *output*
        // normaliser is the one that makes the comparison blind to something,
        // and C1's whole point is that such blindness be visible in review.
        //
        // Conflating them made `exact-bytes` unusable in practice, because
        // every honest suite wants `-bitexact` — the strictest mode was the one
        // no suite could declare, which is precisely backwards.
        if matches!(compare, Compare::ExactBytes { .. }) && !normalise.output.is_empty() {
            return Err(
                "this suite declares output normalisers but mode `exact-bytes`; a \
                 normalised comparison is mode `exact-bytes-normalised` (§1.2 C1), so \
                 that the permitted blindness is visible in review. Invocation \
                 normalisers such as `bitexact` are fine under `exact-bytes`: they \
                 control a variable on both sides rather than hiding a difference"
                    .to_owned(),
            );
        }
        // Rule 2.
        if matches!(compare, Compare::StructuredDiff { .. })
            && string(compare_t, "downgrade_reason").is_none()
        {
            return Err(
                "mode `structured-diff` needs a `downgrade_reason`; C6 is weaker than \
                 C0 by construction and must never launder a failing C0 case (§1.2 C6)"
                    .to_owned(),
            );
        }
        // Rule 3, the filter-tool mirror of rule 2. `raw-exact` (C4) and
        // `raw-tolerant` (C5, with its own mandatory `justification`) are the
        // modes a filter case reaches for by default — pixel data has an
        // exact-or-not answer, the same way C0 does for probe/transcode
        // output. A filter suite that settles for `behavioural` (C7, "did
        // both sides produce a frame", not "was it the same frame") is
        // making the identical move `structured-diff` makes relative to C0:
        // deliberately weaker, and only acceptable with the reason written
        // down where a reviewer sees it. The D7 font-table ceiling
        // (`datascope`/`graphmonitor`/`agraphmonitor`/`pixscope`, see
        // `vaco-filter-scope`'s own doc) is the case this exists for: a
        // permanent, structural reason a filter can never reach `raw-exact`,
        // not a temporary gap — and a suite that has one must say so here,
        // not leave a bare `mode = "behavioural"` a future reader has to
        // reverse-engineer the reason for.
        if tool == Tool::Filter
            && matches!(compare, Compare::Behavioural)
            && string(compare_t, "downgrade_reason").is_none()
        {
            return Err("a `filter`-tool case using mode `behavioural` needs a \
                 `downgrade_reason`: filter output is pixel data with an exact-or-not \
                 answer, so settling for outcome-class-only needs the same declared \
                 justification `structured-diff` needs relative to C0"
                .to_owned());
        }

        Ok(Self {
            name,
            tool,
            tier,
            owner,
            media,
            media_tier,
            axes,
            excludes,
            compare,
            normalise,
            requires: doc
                .get("requires")
                .and_then(Value::as_str_array)
                .unwrap_or_default(),
            timeout: parse_duration(compare_t.get("timeout").and_then(Value::as_str))
                .unwrap_or(crate::run::DEFAULT_TIMEOUT),
        })
    }

    /// Load a suite from a file.
    ///
    /// # Errors
    /// An I/O failure, or anything [`Suite::parse`] rejects.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Expand the matrix into cases.
    ///
    /// A suite with no media yields cases with none — source-filter suites are
    /// legitimate and must not silently produce nothing. A suite whose media
    /// are *all* [`MediaRef::fixed`] (every input is a fixed, named role —
    /// see that field's doc) also yields cases with none as far as this
    /// iteration is concerned; they still resolve via `extra_media`.
    #[must_use]
    pub fn expand(&self) -> Vec<Case> {
        let iterable: Vec<&MediaRef> = self.media.iter().filter(|m| !m.fixed).collect();
        let media_list: Vec<Option<&MediaRef>> = if iterable.is_empty() {
            vec![None]
        } else {
            iterable.into_iter().map(Some).collect()
        };
        let mut out = Vec::new();
        for media in media_list {
            let media_id = media.map_or("none", |m| m.id.as_str());
            for selection in combinations(&self.axes) {
                // `media` is a synthetic pseudo-axis for exclusion matching
                // only — never a real one, so it never contributes argv and
                // never appears in the case id. Without this, a suite that
                // declares several `[[media]]` (an input-container axis in
                // every way but name) would have no way to say "this input
                // does not accept that output", which the reference disagrees
                // with often enough — measured directly, not guessed — that a
                // remux matrix needs it on day one.
                let mut for_matching = selection.clone();
                for_matching.push(("media".to_owned(), media_id.to_owned()));
                if self.excludes.iter().any(|e| e.matches(&for_matching)) {
                    continue;
                }
                let mut argv = Vec::new();
                let mut tier = self.tier;
                let mut case_media: Vec<MediaRef> = media.cloned().into_iter().collect();
                if let Some(t) = media.and_then(|m| self.media_tier.get(&m.id)) {
                    tier = tier.max(*t);
                }
                for (axis_name, value_id) in &selection {
                    if let Some(v) = self
                        .axes
                        .iter()
                        .find(|a| &a.name == axis_name)
                        .and_then(|a| a.values.iter().find(|v| &v.id == value_id))
                    {
                        argv.extend(v.argv.iter().cloned());
                        if let Some(t) = v.tier {
                            tier = tier.max(t);
                        }
                        // Resolved by id against the suite's full `[[media]]`
                        // list, not iterated -- declaring `extra_media` does
                        // not multiply the case count the way another
                        // `[[media]]` entry would. Deduplicated so an id
                        // named by two axes (or already the iterated base
                        // media) does not appear twice in `{media:<id>}`'s
                        // search list.
                        for id in &v.extra_media {
                            if case_media.iter().any(|m| &m.id == id) {
                                continue;
                            }
                            if let Some(m) = self.media.iter().find(|m| &m.id == id) {
                                case_media.push(m.clone());
                            }
                        }
                    }
                }
                out.push(Case {
                    id: CaseId::new(&self.name, media_id, &selection),
                    tool: self.tool,
                    media: case_media,
                    argv,
                    compare: self.compare.clone(),
                    normalise: self.normalise.clone(),
                    requires: self.requires.clone(),
                    timeout: self.timeout,
                    tier,
                });
            }
        }
        out
    }
}

/// Every combination of axis values, in declaration order.
#[must_use]
pub fn combinations(axes: &[Axis]) -> Vec<Vec<(String, String)>> {
    let mut out: Vec<Vec<(String, String)>> = vec![Vec::new()];
    for axis in axes {
        let mut next = Vec::new();
        for prefix in &out {
            for value in &axis.values {
                let mut extended = prefix.clone();
                extended.push((axis.name.clone(), value.id.clone()));
                next.push(extended);
            }
        }
        out = next;
    }
    out
}

/// Discover every suite under `dir`.
///
/// # Errors
/// An I/O failure walking the directory. A malformed suite is returned as a
/// per-file error rather than aborting discovery, so one bad file does not hide
/// every other suite.
pub fn discover(dir: &Path) -> Result<Vec<Result<Suite, String>>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", d.display())),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "toml") {
                out.push(Suite::load(&path));
            }
        }
    }
    Ok(out)
}

/// `"20s"`, `"500ms"`, `"2m"`.
#[must_use]
pub fn parse_duration(s: Option<&str>) -> Option<Duration> {
    let s = s?.trim();
    let (value, mult) = if let Some(v) = s.strip_suffix("ms") {
        (v, 1_u64)
    } else if let Some(v) = s.strip_suffix('s') {
        (v, 1_000)
    } else if let Some(v) = s.strip_suffix('m') {
        (v, 60_000)
    } else {
        (s, 1_000)
    };
    let n: u64 = value.trim().parse().ok()?;
    Some(Duration::from_millis(n.checked_mul(mult)?))
}

fn string(t: &Table, key: &str) -> Option<String> {
    t.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Suite, parse_duration};
    use crate::case::Tier;

    const SUITE: &str = r#"
schema = 1
suite  = "probe-listings"
tool   = "probe"
tier   = "smoke"
owner  = "@correctness-owner"

[[media]]
id     = "none"
source = "builtin://none"

[[axis]]
name = "writer"
values = [
  { id = "default", argv = ["-of", "default"] },
  { id = "json",    argv = ["-of", "json"] },
  { id = "xml",     argv = ["-of", "xml"], tier = "full" },
]

[[axis]]
name = "sections"
values = [
  { id = "pixfmt", argv = ["-show_pixel_formats"] },
]

[[exclude]]
when   = { writer = ["xml"] }
reason = "xml writer is not implemented yet"

[compare]
mode    = "exact-bytes"
capture = ["stdout", "exit-code"]
timeout = "20s"
"#;

    #[test]
    fn a_matrix_expands_to_the_cartesian_product_minus_exclusions() {
        let s = Suite::parse(SUITE).expect("parses");
        let cases = s.expand();
        assert_eq!(cases.len(), 2, "3 writers x 1 section, minus 1 exclusion");
        assert_eq!(
            cases[0].id.as_str(),
            "probe-listings/none/writer=default,sections=pixfmt"
        );
        assert_eq!(cases[0].argv, vec!["-of", "default", "-show_pixel_formats"]);
    }

    #[test]
    fn the_reproduction_command_is_one_line() {
        let s = Suite::parse(SUITE).expect("parses");
        let c = &s.expand()[0];
        assert!(c.reproduction().starts_with("just conformance-run '"));
    }

    #[test]
    fn a_per_value_tier_override_raises_the_case_tier() {
        let text = SUITE.replace("[[exclude]]\nwhen   = { writer = [\"xml\"] }\nreason = \"xml writer is not implemented yet\"\n", "");
        let s = Suite::parse(&text).expect("parses");
        let cases = s.expand();
        let xml = cases
            .iter()
            .find(|c| c.id.as_str().contains("writer=xml"))
            .expect("xml case exists");
        assert_eq!(xml.tier, Tier::Full);
        let json = cases
            .iter()
            .find(|c| c.id.as_str().contains("writer=json"))
            .expect("json case exists");
        assert_eq!(json.tier, Tier::Smoke);
    }

    #[test]
    fn rule_1_normalisers_force_the_normalised_mode() {
        let text = format!("{SUITE}\n[normalise]\noutput = [\"line-endings\"]\n");
        let err = Suite::parse(&text).expect_err("must be rejected");
        assert!(err.contains("exact-bytes-normalised"), "{err}");
    }

    #[test]
    fn rule_1_is_satisfied_by_naming_the_right_mode() {
        let text = format!(
            "{}\n[normalise]\noutput = [\"line-endings\"]\n",
            SUITE.replace(
                "mode    = \"exact-bytes\"",
                "mode = \"exact-bytes-normalised\""
            )
        );
        Suite::parse(&text).expect("the normalised mode is accepted");
    }

    #[test]
    fn rule_2_c6_needs_a_stated_reason() {
        let text = SUITE.replace("mode    = \"exact-bytes\"", "mode = \"structured-diff\"");
        let err = Suite::parse(&text).expect_err("must be rejected");
        assert!(err.contains("downgrade_reason"), "{err}");
    }

    #[test]
    fn an_exclusion_without_a_reason_is_rejected() {
        let text = SUITE.replace("reason = \"xml writer is not implemented yet\"", "");
        assert!(Suite::parse(&text).is_err());
    }

    #[test]
    fn an_exclusion_may_bind_the_synthetic_media_pseudo_axis() {
        let text = r#"
schema = 1
suite  = "remux-probe"
tool   = "transcode"
tier   = "core"
owner  = "@correctness-owner"

[[media]]
id       = "avi-src"
source   = "generated://avi-src.avi"
generate = ["-f", "lavfi", "-i", "testsrc=d=1", "-c:v", "libx264", "-f", "avi"]

[[media]]
id       = "mp4-src"
source   = "generated://mp4-src.mp4"
generate = ["-f", "lavfi", "-i", "testsrc=d=1", "-c:v", "libx264", "-f", "mp4"]

[[axis]]
name = "output"
values = [
  { id = "matroska", argv = ["-f", "matroska"] },
  { id = "mp4",      argv = ["-f", "mp4"] },
]

[[exclude]]
when   = { media = ["avi-src"], output = ["matroska"] }
reason = "OBSERVED (ffmpeg 8.1): AVI's H.264 packets carry no timestamps, and the Matroska muxer refuses to write a packet that lacks one."

[compare]
mode    = "exact-bytes"
capture = ["output-file", "exit-code"]
timeout = "20s"
"#;
        let s = Suite::parse(text).expect("parses");
        let cases = s.expand();
        // 2 media x 2 outputs, minus the one excluded combination.
        assert_eq!(cases.len(), 3, "{cases:#?}");
        assert!(
            !cases
                .iter()
                .any(|c| c.id.as_str() == "remux-probe/avi-src/output=matroska"),
            "the excluded combination must not appear: {cases:#?}"
        );
        assert!(
            cases
                .iter()
                .any(|c| c.id.as_str() == "remux-probe/avi-src/output=mp4"),
            "an unexcluded combination on the same media must still appear"
        );
        assert!(
            cases
                .iter()
                .any(|c| c.id.as_str() == "remux-probe/mp4-src/output=matroska"),
            "the same output excluded for one media must still run for another"
        );
    }

    #[test]
    fn an_axis_with_no_values_is_rejected() {
        let text = SUITE.replace(
            "values = [\n  { id = \"pixfmt\", argv = [\"-show_pixel_formats\"] },\n]",
            "values = []",
        );
        assert!(Suite::parse(&text).is_err());
    }

    #[test]
    fn durations_parse_in_the_manifest_spellings() {
        assert_eq!(
            parse_duration(Some("20s")),
            Some(std::time::Duration::from_secs(20))
        );
        assert_eq!(
            parse_duration(Some("500ms")),
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(
            parse_duration(Some("2m")),
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(parse_duration(Some("nonsense")), None);
        assert_eq!(parse_duration(None), None);
    }

    #[test]
    fn the_shipped_suites_all_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("suites");
        let found = super::discover(&dir).expect("discovery walks the directory");
        assert!(!found.is_empty(), "the crate ships at least one suite");
        for suite in found {
            let suite = suite.expect("every shipped suite must load");
            assert!(
                !suite.expand().is_empty(),
                "{} expands to nothing",
                suite.name
            );
        }
    }
}
