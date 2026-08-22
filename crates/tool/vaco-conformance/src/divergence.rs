//! The divergence allowlist (plan 13 §1.4).
//!
//! # What it is
//!
//! A governed register of "it differs and that's fine", not a suppression file.
//! The distinction is enforced by machinery, because the failure mode of every
//! differential harness is this file quietly growing until the harness proves
//! nothing.
//!
//! # How it works
//!
//! [`Allowlist::parse`] loads `divergences.toml` and rejects, at load time:
//!
//! 1. **Wildcards in scope** — `field = "*"` or `suite = "*"`. A divergence you
//!    cannot localise is a bug you have not understood.
//! 2. **Unknown categories**, and entries whose category is not one of the
//!    seven in §1.4.2.
//! 3. **Missing governance fields** — `justification`, `owner`, `opened`,
//!    `review_by`, `issue`, and the approval count each category demands
//!    (`unexplained` and `upstream-bug` need two).
//! 4. **Expired entries** — `review_by` in the past is a load error, so renewal
//!    is a PR with a fresh justification rather than a silent extension.
//! 5. **Category caps exceeded** — the `[caps]` table, checked against the live
//!    count.
//!
//! At run time [`Allowlist::match_field`] consults the register for one
//! observed difference and increments that entry's **hit counter**, which
//! [`Allowlist::dead_entries`] then uses to propose deletions (mechanism 4) and
//! [`Allowlist::blast_radius`] uses to flag over-broad scopes (mechanism 6).
//!
//! The remaining two mechanisms are process, not code, and the docs say so:
//! CODEOWNERS plus two approvals (5), and publication in the release notes (7).
//!
//! # How to change it
//!
//! Adding a category means a variant in [`Category`], a cap in the `[caps]`
//! table, and a row in the docs explaining what a reviewer must accept. Do not
//! add a category to make an awkward entry fit; that is the dumping-ground
//! failure mode arriving by the front door.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;

use crate::toml::{self, Table, Value};

/// The stable identifier of an allowlist entry, e.g. `DIV-0007`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DivergenceId(pub String);

impl fmt::Display for DivergenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The seven categories of §1.4.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// Strings naming the producing software.
    Identification,
    /// Anything derived from `now()`.
    Wallclock,
    /// Reference-encoder output that is not reproducible run-to-run.
    EncoderNondeterminism,
    /// Documented last-bit differences in float paths, with a numeric bound.
    FloatLastBit,
    /// Reference behaviour that contradicts the specification.
    UpstreamBug,
    /// Temporary: a feature we have not built yet.
    Unimplemented,
    /// Escape hatch of last resort. Hard cap of 10 project-wide.
    Unexplained,
}

impl Category {
    /// Parse the register's spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "identification" => Some(Self::Identification),
            "wallclock" => Some(Self::Wallclock),
            "encoder-nondeterminism" => Some(Self::EncoderNondeterminism),
            "float-lastbit" => Some(Self::FloatLastBit),
            "upstream-bug" => Some(Self::UpstreamBug),
            "unimplemented" => Some(Self::Unimplemented),
            "unexplained" => Some(Self::Unexplained),
            _ => None,
        }
    }

    /// The register's spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identification => "identification",
            Self::Wallclock => "wallclock",
            Self::EncoderNondeterminism => "encoder-nondeterminism",
            Self::FloatLastBit => "float-lastbit",
            Self::UpstreamBug => "upstream-bug",
            Self::Unimplemented => "unimplemented",
            Self::Unexplained => "unexplained",
        }
    }

    /// How many approvals the category demands (§1.4.2, §1.4.3 mechanism 5).
    #[must_use]
    pub const fn required_approvals(self) -> usize {
        match self {
            Self::Unexplained | Self::UpstreamBug => 2,
            _ => 1,
        }
    }

    /// Whether an entry in this category blocks its module being marked done.
    #[must_use]
    pub const fn blocks_module_completion(self) -> bool {
        matches!(self, Self::Unexplained)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an entry is scoped to. Every field is a concrete selector; wildcards
/// are rejected at load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Suite selector. May end in `*` as a prefix match, but may not *be* `*`.
    pub suite: String,
    /// Section of the output, e.g. `format`.
    pub section: Option<String>,
    /// The field that differs. Mandatory unless `box_path` is given.
    pub field: Option<String>,
    /// A container box/element path, for C2 cases.
    pub box_path: Option<String>,
}

impl Scope {
    fn matches(&self, suite: &str, section: Option<&str>, field: &str) -> bool {
        if !suite_matches(&self.suite, suite) {
            return false;
        }
        if let Some(want) = &self.section
            && section != Some(want.as_str())
        {
            return false;
        }
        match &self.field {
            Some(want) => want == field,
            None => self.box_path.as_deref() == Some(field),
        }
    }
}

fn suite_matches(pattern: &str, suite: &str) -> bool {
    pattern
        .strip_suffix('*')
        .map_or_else(|| pattern == suite, |prefix| suite.starts_with(prefix))
}

/// How a difference is recognised.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    /// The values differ and both are non-empty in the declared shapes.
    ValueDiffers {
        /// Substring or prefix our value must contain. Empty means any.
        ours_contains: String,
        /// Substring the reference's value must contain. Empty means any.
        theirs_contains: String,
    },
    /// Both sides stamped a wall-clock time.
    BothAreWallclock {
        /// Maximum permitted skew, in seconds.
        max_skew_seconds: i64,
    },
    /// We emit nothing where the reference emits something.
    WeEmitNothing,
    /// A numerically bounded difference.
    NumericBound {
        /// The bound. Enforced as a maximum, so it cannot silently widen.
        max_abs: f64,
    },
}

impl Rule {
    fn admits(&self, ours: &str, theirs: &str) -> bool {
        match self {
            Self::ValueDiffers {
                ours_contains,
                theirs_contains,
            } => {
                ours != theirs
                    && (ours_contains.is_empty() || ours.contains(ours_contains.as_str()))
                    && (theirs_contains.is_empty() || theirs.contains(theirs_contains.as_str()))
            }
            Self::BothAreWallclock { .. } => ours != theirs,
            Self::WeEmitNothing => ours.is_empty() && !theirs.is_empty(),
            Self::NumericBound { max_abs } => match (ours.parse::<f64>(), theirs.parse::<f64>()) {
                (Ok(a), Ok(b)) => (a - b).abs() <= *max_abs,
                _ => false,
            },
        }
    }

    fn parse(t: &Table) -> Result<Self, String> {
        let kind = t
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("rule needs a `kind`")?;
        match kind {
            "value-differs" => Ok(Self::ValueDiffers {
                ours_contains: t
                    .get("ours_contains")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                theirs_contains: t
                    .get("theirs_contains")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            "both-are-wallclock" => Ok(Self::BothAreWallclock {
                max_skew_seconds: t
                    .get("max_skew_seconds")
                    .and_then(Value::as_int)
                    .unwrap_or(120),
            }),
            "we-emit-nothing" => Ok(Self::WeEmitNothing),
            "numeric-bound" => Ok(Self::NumericBound {
                max_abs: t.get("max_abs").and_then(Value::as_f64).ok_or(
                    "numeric-bound needs a numeric `max_abs`; §1.4.2 requires the \
                            bound to be a number so it cannot silently widen",
                )?,
            }),
            other => Err(format!("unknown rule kind `{other}`")),
        }
    }
}

/// One reviewed divergence.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable identifier.
    pub id: DivergenceId,
    /// One-line summary.
    pub title: String,
    /// Which of the seven categories.
    pub category: Category,
    /// Where it applies.
    pub scope: Scope,
    /// How the difference is recognised.
    pub rule: Rule,
    /// Why a reviewer accepted it.
    pub justification: String,
    /// `YYYY-MM-DD` the entry was opened.
    pub opened: String,
    /// `YYYY-MM-DD` after which CI fails until it is renewed.
    pub review_by: String,
    /// The accountable owner.
    pub owner: String,
    /// Approvers.
    pub approved_by: Vec<String>,
    /// Tracking issue.
    pub issue: String,
    /// How many differences this entry suppressed in the current run.
    hits: Cell<u64>,
}

impl Entry {
    /// Differences this entry suppressed in the current run (mechanism 4/6).
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.get()
    }
}

/// The whole register.
#[derive(Debug)]
pub struct Allowlist {
    entries: Vec<Entry>,
    caps: BTreeMap<Category, u64>,
}

/// The default (empty) register shipped with the harness.
///
/// Empty on purpose. Every entry that ever lands here is a deliberate,
/// reviewed act; seeding it with examples would be seeding it with excuses.
const EMBEDDED_DIVERGENCES: &str = include_str!("../divergences.toml");

impl Allowlist {
    /// Load the register shipped with the harness, or the one `VACO_DIVERGENCES`
    /// names.
    ///
    /// # Errors
    /// Any of the five load-time rejections in the module docs.
    pub fn load() -> Result<Self, String> {
        match std::env::var_os("VACO_DIVERGENCES") {
            Some(p) => {
                let path = std::path::PathBuf::from(p);
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                Self::parse(&text, today())
            }
            None => Self::parse(EMBEDDED_DIVERGENCES, today()),
        }
    }

    /// Parse a register, treating `today` (`YYYY-MM-DD`) as the current date.
    ///
    /// The date is a parameter so expiry is testable without waiting a year.
    ///
    /// # Errors
    /// Any of the five load-time rejections in the module docs.
    pub fn parse(text: &str, today: &str) -> Result<Self, String> {
        let doc = toml::parse(text).map_err(|e| format!("divergences: {e}"))?;
        if doc.get("schema").and_then(Value::as_int) != Some(1) {
            return Err("divergences: `schema` must be 1".to_owned());
        }
        let mut caps = BTreeMap::new();
        if let Some(t) = doc.get("caps").and_then(Value::as_table) {
            for (k, v) in t {
                let cat = Category::parse(k)
                    .ok_or_else(|| format!("divergences: unknown cap category `{k}`"))?;
                let n = v
                    .as_int()
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| format!("divergences: cap for `{k}` must be a count"))?;
                caps.insert(cat, n);
            }
        }
        let mut entries = Vec::new();
        for raw in doc
            .get("divergence")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            let t = raw
                .as_table()
                .ok_or("divergences: [[divergence]] must be a table")?;
            entries.push(entry_from(t, today)?);
        }

        let mut seen = std::collections::BTreeSet::new();
        for e in &entries {
            if !seen.insert(e.id.clone()) {
                return Err(format!("divergences: duplicate id `{}`", e.id));
            }
        }

        let list = Self { entries, caps };
        list.check_caps()?;
        Ok(list)
    }

    /// Mechanism 2: category caps.
    ///
    /// # Errors
    /// A category whose live count exceeds its cap, or a category with entries
    /// and no cap at all — an uncapped category is an uncapped dumping ground.
    pub fn check_caps(&self) -> Result<(), String> {
        let mut live: BTreeMap<Category, u64> = BTreeMap::new();
        for e in &self.entries {
            *live.entry(e.category).or_default() += 1;
        }
        for (cat, count) in live {
            let cap = self
                .caps
                .get(&cat)
                .copied()
                .ok_or_else(|| format!("divergences: category `{cat}` has no cap in [caps]"))?;
            if count > cap {
                return Err(format!(
                    "divergences: category `{cat}` has {count} entries, cap is {cap}"
                ));
            }
        }
        Ok(())
    }

    /// Every entry.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Live count per category, for the lock file and the report.
    #[must_use]
    pub fn live_counts(&self) -> BTreeMap<Category, u64> {
        let mut out = BTreeMap::new();
        for e in &self.entries {
            *out.entry(e.category).or_default() += 1;
        }
        out
    }

    /// Consult the register for one observed difference.
    ///
    /// Returns the entry that admits it, incrementing that entry's hit counter.
    #[must_use]
    pub fn match_field(
        &self,
        suite: &str,
        section: Option<&str>,
        field: &str,
        ours: &str,
        theirs: &str,
    ) -> Option<&Entry> {
        let e = self
            .entries
            .iter()
            .find(|e| e.scope.matches(suite, section, field) && e.rule.admits(ours, theirs))?;
        e.hits.set(e.hits.get() + 1);
        Some(e)
    }

    /// Mechanism 4: entries that suppressed nothing in this run.
    #[must_use]
    pub fn dead_entries(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.hits() == 0).collect()
    }

    /// Mechanism 6: entries suppressing more than `fraction` of `total`.
    #[must_use]
    pub fn blast_radius(&self, total: u64, fraction: f64) -> Vec<(&Entry, f64)> {
        if total == 0 {
            return Vec::new();
        }
        self.entries
            .iter()
            .filter_map(|e| {
                let share = e.hits() as f64 / total as f64;
                (share > fraction).then_some((e, share))
            })
            .collect()
    }

    /// Reset every hit counter, so a fresh run reports fresh numbers.
    pub fn reset_hits(&self) {
        for e in &self.entries {
            e.hits.set(0);
        }
    }
}

fn entry_from(t: &Table, today: &str) -> Result<Entry, String> {
    let need = |key: &str| -> Result<String, String> {
        t.get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("divergences: entry is missing `{key}`"))
    };
    let id = DivergenceId(need("id")?);
    let category = Category::parse(&need("category")?)
        .ok_or_else(|| format!("divergences: {id} has an unknown category"))?;

    let scope_t = t
        .get("scope")
        .and_then(Value::as_table)
        .ok_or_else(|| format!("divergences: {id} needs a `scope`"))?;
    let scope = scope_from(scope_t, &id)?;

    let rule = Rule::parse(
        t.get("rule")
            .and_then(Value::as_table)
            .ok_or_else(|| format!("divergences: {id} needs a `rule`"))?,
    )
    .map_err(|e| format!("divergences: {id}: {e}"))?;

    let justification = need("justification")?;
    if justification.len() < 40 {
        return Err(format!(
            "divergences: {id} has a justification of {} characters; §1.4.2 expects an \
             argument a reviewer can accept, not a note to self",
            justification.len()
        ));
    }
    let opened = need("opened")?;
    let review_by = need("review_by")?;
    // Mechanism 3: expiry.
    if review_by.as_str() < today {
        return Err(format!(
            "divergences: {id} expired on {review_by}; renew it with a fresh \
             justification or delete it (§1.4.3 mechanism 3)"
        ));
    }
    let approved_by = t
        .get("approved_by")
        .and_then(Value::as_str_array)
        .unwrap_or_default();
    if approved_by.len() < category.required_approvals() {
        return Err(format!(
            "divergences: {id} is `{category}` and needs {} approvals, has {}",
            category.required_approvals(),
            approved_by.len()
        ));
    }

    Ok(Entry {
        id,
        title: need("title")?,
        category,
        scope,
        rule,
        justification,
        opened,
        review_by,
        owner: need("owner")?,
        approved_by,
        issue: need("issue")?,
        hits: Cell::new(0),
    })
}

fn scope_from(t: &Table, id: &DivergenceId) -> Result<Scope, String> {
    let get = |key: &str| t.get(key).and_then(Value::as_str).map(str::to_owned);
    let suite = get("suite").ok_or_else(|| format!("divergences: {id} scope needs a `suite`"))?;
    // Mechanism 1: no wildcards.
    for (key, value) in [
        ("suite", Some(&suite)),
        ("section", get("section").as_ref()),
        ("field", get("field").as_ref()),
        ("box_path", get("box_path").as_ref()),
    ] {
        if let Some(v) = value
            && (v == "*" || v.is_empty())
        {
            return Err(format!(
                "divergences: {id} scope `{key}` is a wildcard; §1.4.3 mechanism 1 \
                 requires a concrete selector — a divergence you cannot localise is a \
                 bug you have not understood"
            ));
        }
    }
    let field = get("field");
    let box_path = get("box_path");
    if field.is_none() && box_path.is_none() {
        return Err(format!(
            "divergences: {id} scope needs a `field` or a `box_path`"
        ));
    }
    Ok(Scope {
        suite,
        section: get("section"),
        field,
        box_path,
    })
}

/// Today's date as `YYYY-MM-DD`, UTC.
///
/// Computed from the epoch rather than pulled from a date library: the harness
/// has no dependency budget for one, and civil-from-days is fifteen lines of
/// arithmetic from Howard Hinnant's published algorithm.
#[must_use]
pub fn today() -> &'static str {
    use std::sync::OnceLock;
    static TODAY: OnceLock<String> = OnceLock::new();
    TODAY.get_or_init(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_i64, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
        format!("{y:04}-{m:02}-{d:02}")
    })
}

/// Days since 1970-01-01 to `(year, month, day)`.
///
/// Hinnant, *`chrono`-Compatible Low-Level Date Algorithms* (public domain).
#[must_use]
#[expect(
    clippy::integer_division,
    reason = "truncating division is the algorithm; every divisor is a nonzero \
              constant and the remainders are taken explicitly where needed"
)]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Allowlist, Category, civil_from_days};

    const TODAY: &str = "2026-08-21";

    fn entry(extra: &str) -> String {
        format!(
            "schema = 1\n\
             [caps]\nidentification = 40\nunexplained = 10\n\n\
             [[divergence]]\n\
             id = \"DIV-0001\"\n\
             title = \"format_long_name differs\"\n\
             category = \"identification\"\n\
             scope = {{ suite = \"probe-*\", section = \"format\", field = \"format_long_name\" }}\n\
             rule = {{ kind = \"value-differs\" }}\n\
             justification = \"Descriptive prose authored by FFmpeg; we author our own long names and match format_name exactly.\"\n\
             opened = 2026-08-01\n\
             review_by = 2027-08-01\n\
             owner = \"@correctness-owner\"\n\
             approved_by = [\"@correctness-owner\"]\n\
             issue = \"vaco#412\"\n{extra}"
        )
    }

    #[test]
    fn a_well_formed_entry_loads_and_matches() {
        let list = Allowlist::parse(&entry(""), TODAY).expect("loads");
        assert_eq!(list.entries().len(), 1);
        let hit = list
            .match_field(
                "probe-isobmff",
                Some("format"),
                "format_long_name",
                "Vaco MP4",
                "QuickTime / MOV",
            )
            .expect("matches");
        assert_eq!(hit.category, Category::Identification);
        assert_eq!(hit.hits(), 1);
        // A different field is not covered.
        assert!(
            list.match_field("probe-isobmff", Some("format"), "format_name", "mov", "mp4")
                .is_none()
        );
    }

    #[test]
    fn mechanism_1_wildcards_are_rejected() {
        let text = entry("").replace("field = \"format_long_name\"", "field = \"*\"");
        let err = Allowlist::parse(&text, TODAY).expect_err("wildcard must be rejected");
        assert!(err.contains("wildcard"), "{err}");

        let text = entry("").replace("suite = \"probe-*\"", "suite = \"*\"");
        assert!(Allowlist::parse(&text, TODAY).is_err());
    }

    #[test]
    fn mechanism_2_caps_are_enforced() {
        let text = entry("").replace("identification = 40", "identification = 0");
        let err = Allowlist::parse(&text, TODAY).expect_err("cap must be enforced");
        assert!(err.contains("cap is 0"), "{err}");
    }

    #[test]
    fn an_uncapped_category_is_rejected() {
        let text = entry("").replace("identification = 40\n", "");
        let err = Allowlist::parse(&text, TODAY).expect_err("uncapped must be rejected");
        assert!(err.contains("no cap"), "{err}");
    }

    #[test]
    fn mechanism_3_expiry_fails_the_load() {
        let text = entry("").replace("review_by = 2027-08-01", "review_by = 2026-01-01");
        let err = Allowlist::parse(&text, TODAY).expect_err("expired must be rejected");
        assert!(err.contains("expired"), "{err}");
    }

    #[test]
    fn mechanism_4_dead_entries_are_reported() {
        let list = Allowlist::parse(&entry(""), TODAY).expect("loads");
        assert_eq!(list.dead_entries().len(), 1, "nothing has been hit yet");
        let _ = list.match_field("probe-x", Some("format"), "format_long_name", "a", "b");
        assert!(list.dead_entries().is_empty());
        list.reset_hits();
        assert_eq!(list.dead_entries().len(), 1);
    }

    #[test]
    fn mechanism_6_blast_radius_flags_broad_scopes() {
        let list = Allowlist::parse(&entry(""), TODAY).expect("loads");
        for _ in 0..30 {
            let _ = list.match_field("probe-x", Some("format"), "format_long_name", "a", "b");
        }
        assert_eq!(list.blast_radius(1000, 0.02).len(), 1);
        assert!(list.blast_radius(10_000, 0.02).is_empty());
    }

    #[test]
    fn unexplained_needs_two_approvals() {
        let text = entry("").replace(
            "category = \"identification\"",
            "category = \"unexplained\"",
        );
        let err = Allowlist::parse(&text, TODAY).expect_err("one approval is not enough");
        assert!(err.contains("needs 2 approvals"), "{err}");
        assert!(Category::Unexplained.blocks_module_completion());
    }

    #[test]
    fn a_thin_justification_is_rejected() {
        let text = entry("").replace(
            "justification = \"Descriptive prose authored by FFmpeg; we author our own long names and match format_name exactly.\"",
            "justification = \"it differs\"",
        );
        let err = Allowlist::parse(&text, TODAY).expect_err("must be rejected");
        assert!(err.contains("justification"), "{err}");
    }

    #[test]
    fn a_numeric_bound_must_be_a_number() {
        let text = entry("").replace(
            "rule = { kind = \"value-differs\" }",
            "rule = { kind = \"numeric-bound\" }",
        );
        assert!(Allowlist::parse(&text, TODAY).is_err());
    }

    #[test]
    fn the_shipped_register_loads() {
        let list = Allowlist::load().expect("the shipped register must always load");
        list.check_caps().expect("caps hold");
    }

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_744), (2026, 10, 18));
    }
}
