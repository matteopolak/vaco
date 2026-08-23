//! D15 / plan 13 §6: the clean-room evidence trail, made mechanical.
//!
//! A clean-room claim is only worth what its record can show. This gate is that
//! record's enforcement, and it checks two independent things.
//!
//! **Tables.** Every `static`/`const` array of 32 or more elements in a codec,
//! format, filter, signal or model crate must name where it came from, in that
//! crate's `provenance/<crate>.toml`. Large constant tables are the single most
//! contested artefact in a reimplementation — format-dictated ones fall under
//! merger, authorial ones do not, and the difference is a question of *fact*
//! about where the numbers came from. Answering it years later from memory is
//! not possible; answering it from a file written the same day is trivial.
//!
//! The check runs both ways. A table with no entry fails, and an entry naming a
//! table that no longer exists fails too — a record that quietly rots into
//! fiction is worse than no record, because it reads as evidence.
//!
//! **Trailers.** Every commit touching implementation code must carry
//! `Signed-off-by`, `Vaco-Provenance` from a fixed enum, and — when the
//! provenance is a document — a `Vaco-Spec-Ref` whose source id resolves to a
//! `[[source]]` we actually recorded acquiring. A citation to a document nobody
//! logged looks authoritative and proves nothing.
//!
//! History before `provenance/baseline` is exempt. The alternative was to
//! rewrite every existing commit message, which would have produced trailers
//! written from memory long after the fact — exactly the false record §6 warns
//! against. The baseline is honest: it says the machine-checked trail starts
//! here.

use crate::{Map, Set, capture, repo_root, rust_files};
use std::path::{Path, PathBuf};

/// Arrays at or above this many elements need a provenance entry.
///
/// Plan 13 §6.4 suggests 32. Below it a table is a handful of magic numbers
/// that the surrounding code explains; above it, it is a transcription of
/// something, and which something is the whole question.
const TABLE_THRESHOLD: usize = 32;

/// Crate areas whose constant tables carry provenance risk.
///
/// `app`, `io`, `tool` and `registry` are excluded: their tables are our own
/// option lists and generated output, not transcriptions of anyone's document.
const AREAS: &[&str] = &["codec", "format", "filter", "signal", "model"];

/// The `Vaco-Provenance` enum (plan 13 §6.2).
const KINDS: &[&str] = &["spec", "rfc", "paper", "blackbox", "original"];

/// Provenance kinds that must be backed by a `Vaco-Spec-Ref`.
const CITED: &[&str] = &["spec", "rfc", "paper"];

/// How a table's numbers reached the file.
///
/// `kind` on the source says what the document is; `method` on the table says
/// what we did with it, and the two answer different questions. "Transcribed
/// from ITU-T H.264 Table 9-44" and "derived by evaluating the standard's
/// equation" are both `spec`, and only one of them would survive somebody
/// finding an arithmetic error in the other.
const METHODS: &[&str] = &["transcribed", "derived", "probed", "original"];

/// Paths whose commits must carry the full trailer block.
const CODE_PATHS: &[&str] = &[
    "crates/codec/",
    "crates/format/",
    "crates/filter/",
    "crates/signal/",
];

pub fn run(check: bool) -> crate::Task {
    let root = repo_root();
    let mut findings = Vec::new();

    findings.extend(resolve(&root)?);
    findings.extend(tables(&root)?);
    findings.extend(trailers(&root)?);

    if findings.is_empty() {
        println!("provenance-check: OK");
        return Ok(());
    }
    if check {
        // `--check` reports without failing, for the pre-commit hook.
        for f in &findings {
            println!("provenance-check: {f}");
        }
        return Ok(());
    }
    Err(findings.join("\n"))
}

// ---------------------------------------------------------------- tables

/// One recorded source document.
struct Source {
    kind: String,
}

/// Read one `provenance/*.toml` into (sources it declares, tables it records).
///
/// A table's `source` is **not** resolved here. Documents are declared once
/// anywhere under `provenance/` and cited from everywhere, because the same
/// standard backs several crates — ISO/IEC 14496-12 backs the MP4 demuxer, the
/// MP4 muxer and the shared ISOBMFF crate — and declaring it once per crate
/// would be three records of one acquisition, which is the failure mode this
/// whole directory exists to avoid (D19). [`resolve`] does the checking against
/// the union.
fn record(path: &Path) -> Result<(Map<String, Source>, Map<(String, String), usize>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let parsed = crate::toml::tables(&text, &["source", "table"])
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let mut sources = Map::new();
    let mut rows = Map::new();
    for t in &parsed {
        if t.name == "source" {
            let id = t
                .need("id")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let kind = t
                .need("kind")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if !KINDS.contains(&kind) {
                return Err(format!(
                    "{}: line {}: kind `{kind}` is not one of {}",
                    path.display(),
                    t.origin_line,
                    KINDS.join(" | ")
                ));
            }
            t.need("title")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            t.need("acquired")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            sources.insert(
                id.to_owned(),
                Source {
                    kind: kind.to_owned(),
                },
            );
        } else {
            let name = t
                .need("name")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let file = t
                .need("file")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            t.need("source")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let method = t
                .need("method")
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if !METHODS.contains(&method) {
                return Err(format!(
                    "{}: line {}: table `{name}` has method `{method}`, not one of {}",
                    path.display(),
                    t.origin_line,
                    METHODS.join(" | ")
                ));
            }
            rows.insert((file.to_owned(), name.to_owned()), t.origin_line);
        }
    }
    Ok((sources, rows))
}

/// Check every `[[table]]` row's `source` and `clause` against the union
/// register. Separated from parsing because the register is not complete until
/// every file has been read.
fn resolve(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("provenance");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();

    let mut register: Map<String, Source> = Map::new();
    for f in &files {
        let (sources, _) = record(f)?;
        for (id, src) in sources {
            if register.insert(id.clone(), src).is_some() {
                return Err(format!(
                    "{}: source `{id}` is declared more than once under provenance/ \
                     — one document, one record",
                    f.display()
                ));
            }
        }
    }

    let mut findings = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        for t in crate::toml::tables(&text, &["source", "table"])
            .map_err(|e| format!("{}: {e}", f.display()))?
        {
            if t.name != "table" {
                continue;
            }
            let (Some(name), Some(src)) = (t.get("name"), t.get("source")) else {
                continue;
            };
            let Some(source) = register.get(src) else {
                findings.push(format!(
                    "{}: line {}: table `{name}` cites source `{src}`, which no \
                     `[[source]]` under provenance/ declares",
                    f.display(),
                    t.origin_line
                ));
                continue;
            };
            if CITED.contains(&source.kind.as_str()) && t.get("clause").is_none_or(str::is_empty) {
                findings.push(format!(
                    "{}: line {}: table `{name}` cites the document `{src}` but names \
                     no `clause` — a document reference without a clause cannot be \
                     checked by a human either",
                    f.display(),
                    t.origin_line
                ));
            }
        }
    }
    Ok(findings)
}

fn tables(root: &Path) -> Result<Vec<String>, String> {
    let mut findings = Vec::new();
    let dir = root.join("provenance");

    for (area, krate, path) in crate::crates() {
        if !AREAS.contains(&area.as_str()) {
            continue;
        }
        let found = large_tables(&path.join("src"), root);
        let file = dir.join(format!("{krate}.toml"));

        if found.is_empty() {
            if file.exists() {
                let (_, rows) = record(&file)?;
                for ((f, n), line) in rows {
                    findings.push(format!(
                        "{}: line {line}: records table `{n}` in {f}, which no longer \
                         exists — delete the entry rather than leaving a record that \
                         reads as evidence",
                        file.display()
                    ));
                }
            }
            continue;
        }
        if !file.exists() {
            findings.push(format!(
                "{krate} has {} constant table(s) of {TABLE_THRESHOLD}+ elements and no \
                 provenance/{krate}.toml: {}",
                found.len(),
                found
                    .iter()
                    .map(|(f, n, c)| format!("{n} ({c} elements, {f})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }

        let (_, rows) = record(&file)?;
        let present: Set<(String, String)> = found
            .iter()
            .map(|(f, n, _)| (f.clone(), n.clone()))
            .collect();

        for (f, n, c) in &found {
            if !rows.contains_key(&(f.clone(), n.clone())) {
                findings.push(format!(
                    "{f}: `{n}` has {c} elements and no `[[table]]` entry in \
                     provenance/{krate}.toml — say where the numbers came from"
                ));
            }
        }
        for ((f, n), line) in &rows {
            if !present.contains(&(f.clone(), n.clone())) {
                findings.push(format!(
                    "{}: line {line}: records table `{n}` in {f}, which no longer has \
                     {TABLE_THRESHOLD}+ elements or no longer exists",
                    file.display()
                ));
            }
        }
    }
    Ok(findings)
}

/// Every `static`/`const` array of `TABLE_THRESHOLD`+ elements, outside tests.
///
/// Returned as (repo-relative file, item name, element count).
fn large_tables(src: &Path, root: &Path) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for f in rust_files(src) {
        // `#[cfg(test)] mod tests;` puts a whole file behind the attribute, which
        // the in-file scanner cannot see from inside that file.
        if f.file_name().is_some_and(|n| n == "tests.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .to_string_lossy()
            .into_owned();
        for (name, count) in scan(&text) {
            if count >= TABLE_THRESHOLD {
                out.push((rel.clone(), name, count));
            }
        }
    }
    out.sort();
    out
}

/// Find the array items in one file, skipping `#[cfg(test)]` regions.
///
/// A deliberately small parser: it tracks brace depth to know when a
/// `#[cfg(test)]` item ends, then hands each `static`/`const` whose type starts
/// with `[` or `&[` to [`item`].
///
/// It walks **byte** indices, not character indices. The first version mixed
/// the two — a `Vec<char>` for the scan and `text.get(i..)` for the lookahead —
/// which silently agrees with itself until a file contains a non-ASCII
/// character, and then every item after that character is invisible. It cost
/// the largest table in the repository, `vaco-pixfmt`'s 267 descriptors, which
/// the gate reported as absent while cheerfully passing on the rest. Everything
/// this scanner matches on is ASCII, so bytes are the right unit throughout.
fn scan(text: &str) -> Vec<(String, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // Depth at which the enclosing `#[cfg(test)]` item began, if we are in one.
    let mut test_from: Option<i32> = None;
    let mut depth: i32 = 0;
    let mut pending_test = false;

    while i < b.len() {
        if text.is_char_boundary(i) && text.get(i..).is_some_and(|s| s.starts_with("#[cfg(test)]"))
        {
            pending_test = true;
        }
        match b.get(i).copied() {
            Some(b'{') => {
                if pending_test && test_from.is_none() {
                    test_from = Some(depth);
                    pending_test = false;
                }
                depth += 1;
            }
            Some(b'}') => {
                depth -= 1;
                if test_from == Some(depth) {
                    test_from = None;
                }
            }
            _ => {}
        }

        if test_from.is_none()
            && text.is_char_boundary(i)
            && let Some(rest) = text.get(i..)
            && (rest.starts_with("static ") || rest.starts_with("const "))
            && (i == 0
                || b.get(i - 1)
                    .is_some_and(|p| !p.is_ascii_alphanumeric() && *p != b'_'))
            && let Some((name, count)) = item(rest)
        {
            out.push((name, count));
        }
        i += 1;
    }
    out
}

/// Parse one `static NAME: [T; N] = [ … ];` header, returning (name, elements).
fn item(rest: &str) -> Option<(String, usize)> {
    let after_kw = rest.split_once(' ')?.1;
    let after_kw = after_kw.trim_start_matches("mut ").trim_start();
    let (name, tail) = after_kw.split_once(':')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let ty = tail.trim_start();
    if !(ty.starts_with('[') || ty.starts_with("&[")) {
        return None;
    }
    let eq = tail.find('=')?;
    let init = tail.get(eq + 1..)?;
    Some((name.to_owned(), elements(init)?))
}

/// Count commas at depth 1 of the first bracketed group in `init`.
///
/// Comments and literals are skipped rather than counted. That is not
/// fastidiousness: `vaco-pixfmt`'s 267-entry descriptor table carries `//`
/// comments containing `]`, and a counter that reads those as brackets closes
/// the array early, returns a number below the threshold, and drops the largest
/// table in the repository **silently**. A gate whose failure mode is a quiet
/// false negative is worse than no gate, so this one is written to lex.
fn elements(init: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut n = 0usize;
    // Content seen since the last separator. Rust arrays almost always carry a
    // trailing comma, so counting commas and adding one reports 268 for
    // `[PixFmtDescriptor; 267]` — close enough to look right and wrong enough
    // to make the gate's own error messages untrustworthy.
    let mut any = false;
    let b: Vec<char> = init.chars().collect();
    let mut i = 0usize;

    while i < b.len() {
        let c = b.get(i).copied()?;
        let next = b.get(i + 1).copied().unwrap_or('\0');

        if c == '/' && next == '/' {
            while i < b.len() && b.get(i).copied() != Some('\n') {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == '*' {
            i += 2;
            while i + 1 < b.len()
                && !(b.get(i).copied() == Some('*') && b.get(i + 1).copied() == Some('/'))
            {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' {
            i += 1;
            while i < b.len() {
                match b.get(i).copied() {
                    Some('\\') => i += 2,
                    Some('"') => break,
                    _ => i += 1,
                }
            }
            i += 1;
            any = true;
            continue;
        }
        // A char literal, distinguished from a lifetime by its closing quote.
        if c == '\'' && (next == '\\' || b.get(i + 2).copied() == Some('\'')) {
            i += if next == '\\' { 4 } else { 3 };
            any = true;
            continue;
        }

        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(if any { n + 1 } else { n });
                }
            }
            ',' if depth == 1 => {
                if any {
                    n += 1;
                }
                any = false;
            }
            c if !c.is_whitespace() && depth >= 1 => any = true,
            _ => {}
        }
        i += 1;
    }
    None
}

// -------------------------------------------------------------- trailers

fn trailers(root: &Path) -> Result<Vec<String>, String> {
    let baseline_file = root.join("provenance/baseline");
    let Ok(baseline) = std::fs::read_to_string(&baseline_file) else {
        return Ok(vec![format!(
            "{} is missing — it names the commit from which trailers are checked",
            baseline_file.display()
        )]);
    };
    let baseline = baseline
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or_default()
        .to_owned();
    if baseline.is_empty() {
        return Ok(vec![format!("{}: no commit id", baseline_file.display())]);
    }

    // A shallow or freshly-cloned tree may not contain the baseline.
    if capture(
        std::process::Command::new("git")
            .args(["cat-file", "-e", &format!("{baseline}^{{commit}}")])
            .current_dir(root),
    )
    .is_err()
    {
        println!("provenance-check: baseline {baseline} not in this tree, skipping trailers");
        return Ok(Vec::new());
    }

    let sources = all_source_ids(root)?;
    let log = capture(
        std::process::Command::new("git")
            .args([
                "log",
                "--no-merges",
                "--format=%H%x00%B%x00%x00",
                &format!("{baseline}..HEAD"),
            ])
            .current_dir(root),
    )?;

    let mut findings = Vec::new();
    for entry in log.split("\0\0") {
        let entry = entry.trim_start_matches('\n');
        let Some((sha, body)) = entry.split_once('\0') else {
            continue;
        };
        if sha.trim().is_empty() {
            continue;
        }
        let sha = sha.trim();
        let short = sha.get(..8).unwrap_or(sha);

        let files = capture(
            std::process::Command::new("git")
                .args(["show", "--name-only", "--format=", sha])
                .current_dir(root),
        )?;
        let touches_code = files
            .lines()
            .any(|f| CODE_PATHS.iter().any(|p| f.starts_with(p)));

        if !body.lines().any(|l| l.starts_with("Signed-off-by:")) {
            findings.push(format!("{short}: no `Signed-off-by:` trailer"));
        }
        if !touches_code {
            continue;
        }
        // Both trailers may repeat. A single-value rule was the first design
        // and it broke on the first commit that aggregated a wave: fifteen
        // crates implemented from a dozen documents do not have *one*
        // provenance, and forcing them to pick one would have made the record
        // less true rather than more.
        let kinds = values(body, "Vaco-Provenance");
        if kinds.is_empty() {
            findings.push(format!(
                "{short}: touches implementation code and has no `Vaco-Provenance:` trailer"
            ));
            continue;
        }
        let mut wants_citation = false;
        for kind in &kinds {
            let base = kind.split(':').next().unwrap_or(kind);
            if base != "cleanroom-doc" && !KINDS.contains(&base) {
                findings.push(format!(
                    "{short}: `Vaco-Provenance: {kind}` is not one of {} | cleanroom-doc:<path>",
                    KINDS.join(" | ")
                ));
            }
            wants_citation |= CITED.contains(&base);
        }
        let refs = values(body, "Vaco-Spec-Ref");
        if wants_citation && refs.is_empty() {
            findings.push(format!(
                "{short}: `Vaco-Provenance: {}` needs at least one `Vaco-Spec-Ref:` trailer",
                kinds.join(", ")
            ));
        }
        for r in &refs {
            let id = r.split_whitespace().next().unwrap_or_default();
            if !sources.contains(id) {
                findings.push(format!(
                    "{short}: `Vaco-Spec-Ref: {r}` starts with `{id}`, which no \
                     `[[source]]` in provenance/ declares — a citation to a document \
                     we never recorded acquiring proves nothing"
                ));
            }
        }
    }
    Ok(findings)
}

/// Every value of a trailer key, in order. A key may legitimately repeat.
fn values(body: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    body.lines()
        .filter_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Validate one commit message's trailers without consulting git.
///
/// The `commit-msg` hook's entry point: it is the only moment at which a bad
/// trailer costs nothing to fix. The first version of this gate had no such
/// hook, and the placeholder `prepare-commit-msg` writes went straight into a
/// pushed commit, where fixing it meant a rewrite.
///
/// # Errors
/// One line per problem.
pub fn check_message(root: &Path, body: &str, touches_code: bool) -> Result<(), String> {
    let sources = all_source_ids(root)?;
    let mut findings = Vec::new();
    if !body.lines().any(|l| l.starts_with("Signed-off-by:")) {
        findings.push("no `Signed-off-by:` trailer".to_owned());
    }
    if touches_code {
        let kinds = values(body, "Vaco-Provenance");
        if kinds.is_empty() {
            findings.push(
                "touches implementation code and has no `Vaco-Provenance:` trailer".to_owned(),
            );
        }
        let mut wants_citation = false;
        for kind in &kinds {
            let base = kind.split(':').next().unwrap_or(kind);
            if base != "cleanroom-doc" && !KINDS.contains(&base) {
                findings.push(format!(
                    "`Vaco-Provenance: {kind}` is not one of {} | cleanroom-doc:<path>",
                    KINDS.join(" | ")
                ));
            }
            wants_citation |= CITED.contains(&base);
        }
        let refs = values(body, "Vaco-Spec-Ref");
        if wants_citation && refs.is_empty() {
            findings.push("a document provenance needs a `Vaco-Spec-Ref:` trailer".to_owned());
        }
        for r in &refs {
            let id = r.split_whitespace().next().unwrap_or_default();
            if !sources.contains(id) {
                findings.push(format!(
                    "`Vaco-Spec-Ref: {r}` starts with `{id}`, which no `[[source]]` in \
                     provenance/ declares. Known ids: {}",
                    {
                        let mut v: Vec<&str> = sources.iter().map(String::as_str).collect();
                        v.sort_unstable();
                        v.join(" ")
                    }
                ));
            }
        }
    }
    if findings.is_empty() {
        Ok(())
    } else {
        Err(findings.join("\n"))
    }
}

fn all_source_ids(root: &Path) -> Result<Set<String>, String> {
    let mut ids = Set::new();
    let dir = root.join("provenance");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(ids);
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    for f in files {
        let (sources, _) = record(&f)?;
        ids.extend(sources.into_keys());
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_a_slice_literal() {
        let v = scan("pub static T: &[u8] = &[1, 2, 3];");
        assert_eq!(v, [("T".to_owned(), 3)]);
    }

    #[test]
    fn ignores_items_inside_a_test_module() {
        let src = "#[cfg(test)]\nmod tests {\n    const BIG: [u8; 3] = [1, 2, 3];\n}\n";
        assert!(scan(src).is_empty(), "{:?}", scan(src));
    }

    #[test]
    fn ignores_a_const_generic_parameter() {
        assert!(scan("fn f<const N: usize>() {}").is_empty());
    }

    #[test]
    fn a_bracket_inside_a_comment_does_not_close_the_array() {
        let src = "const T: [u8; 3] = [\n 1, // see [1]\n 2,\n 3,\n];";
        assert_eq!(scan(src), [("T".to_owned(), 3)]);
    }

    #[test]
    fn a_bracket_inside_a_string_does_not_close_the_array() {
        let src = "const T: [&str; 2] = [\"a]b\", \"c\"];";
        assert_eq!(scan(src), [("T".to_owned(), 2)]);
    }

    #[test]
    fn a_nested_array_counts_rows_not_cells() {
        let v = scan("const M: [[u8; 2]; 3] = [[1, 2], [3, 4], [5, 6]];");
        assert_eq!(v, [("M".to_owned(), 3)]);
    }
}
