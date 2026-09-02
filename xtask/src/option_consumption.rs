//! A `CliOptionTable` entry that parses and does nothing.
//!
//! # Why this exists
//!
//! `vaco-cli-core`'s `FfmpegOptions`/`FfprobeOptions` (see
//! `crates/app/vaco-cli-core/src/tables/{ffmpeg,ffprobe}.rs`) declare every
//! argv flag `vaco`/`vaco-probe` accept. Accepting a flag and *acting* on it
//! are different claims: a manual audit found 102 of 172 `ffmpeg` options and
//! 8 of 65 `ffprobe` options that parsed successfully and then were read by
//! no code at all — the flag was silently a no-op. `-filter_complex_script`
//! was the worst instance found: it hands over an entire filtergraph and the
//! build proceeds to filter nothing, exiting 0 with a plausible-looking
//! output. `crate::cli::refuse_unimplemented_options`'s hand-maintained name
//! list exists to turn each *known* instance of this into a loud error
//! instead of a silent one.
//!
//! # Why this cannot be a `CliOptionTable` compile error
//!
//! `#[derive(CliOptionTable)]` expands against the syntax of the one enum it
//! is attached to, inside `vaco-cli-core`. "Consumed" is a fact about a
//! *different* crate: `vaco-cli`/`vaco-probe` dispatch by comparing
//! `ParsedOption::resolved().0` (a `String` computed at argv-parse time)
//! against string literals scattered across `match` arms and `if` chains in
//! `cli.rs`/`exec.rs`. A proc-macro sees only the `TokenStream` of its own
//! input; it has no view into another crate's source, and nothing in that
//! dispatch shape gives rustc's own exhaustiveness checking anything to
//! chew on (there is no `enum` being matched, just strings). Closing that
//! gap for real would mean rebuilding `vaco-cli`'s dispatch around a typed,
//! generated enum matched exhaustively — a rewrite of `cli.rs`/`exec.rs`
//! itself, not an extension of the derive, and squarely the file another
//! agent is actively editing for the equivalent-in-spirit
//! `refuse_unimplemented_options` list.
//!
//! # What this does instead
//!
//! The same shape of gap already exists for components
//! (`reachability_check`) and public API (`dead_code`), and both are
//! answered the same way this one is: a mechanical, re-runnable, name-based
//! sweep over the checked-in source text, reported rather than enforced,
//! because a textual scan cannot prove a name is truly unread (it could be
//! matched through a helper, a re-export, or a macro) any more than it could
//! prove one of those is truly unregistered. It turns "found once by a
//! manual audit" into "found every time this command runs," which is the
//! actual value the audit had.
//!
//! Aliases are resolved to their target before the check: `-vcodec` is
//! `alias_of = "codec"`, `codec` is `alias_of = "c"`, so what must appear in
//! `vaco-cli`/`vaco-probe`'s source is `"c"`, not `"vcodec"` — matching
//! `ParsedOption::resolved()`'s own semantics, where every alias in a chain
//! collapses to one dispatch key. `CliOptionTable` itself already refuses to
//! generate a chain longer than one hop, so following `alias_of` exactly
//! once always reaches the real key.

use crate::{Set, Task, repo_root};
use std::path::{Path, PathBuf};

/// One `#[cli(...)]` attribute's `name` and (if present) `alias_of`.
struct Entry {
    name: String,
    alias_of: Option<String>,
}

/// Pull every `#[cli(...)]` attribute's `name = "..."` and `alias_of = "..."`
/// out of a `CliOptionTable` derive input, by balanced-paren scanning rather
/// than a full parser — this binary stays dependency-free (see the module
/// doc on `main.rs`), and the attribute body's only nested parens are
/// `flags(...)`, which a naive `[^)]*` regex cannot skip over.
fn parse_cli_attrs(text: &str) -> Vec<Entry> {
    let marker = "#[cli(";
    let mut entries = Vec::new();
    let mut i = 0;
    while let Some(rel) = text.get(i..).and_then(|s| s.find(marker)) {
        let open = i + rel + marker.len() - 1; // index of the '('
        let bytes = text.as_bytes();
        let mut depth: i32 = 0;
        let mut close = open;
        let mut k = open;
        while k < bytes.len() {
            match bytes[k] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = k;
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        let Some(body) = text.get(open + 1..close) else {
            break;
        };
        if let Some(name) = extract_str_field(body, "name") {
            let alias_of = extract_str_field(body, "alias_of");
            entries.push(Entry { name, alias_of });
        }
        i = close + 1;
    }
    entries
}

/// Find `field = "value"` in an attribute body and return `value`.
fn extract_str_field(body: &str, field: &str) -> Option<String> {
    let key_pos = {
        let mut idx = None;
        let mut search_from = 0;
        while let Some(rel) = body.get(search_from..).and_then(|s| s.find(field)) {
            let pos = search_from + rel;
            let before_ok = pos == 0
                || !body
                    .as_bytes()
                    .get(pos.wrapping_sub(1))
                    .is_some_and(u8::is_ascii_alphanumeric);
            let after = body.get(pos + field.len()..).unwrap_or("").trim_start();
            if before_ok && after.starts_with('=') {
                idx = Some(pos + field.len());
                break;
            }
            search_from = pos + field.len();
        }
        idx?
    };
    let rest = body.get(key_pos..)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest.get(..end)?.to_string())
}

/// Resolve one alias hop, matching `gen_cli::resolve`'s own refusal to chain
/// aliases: a variant's `alias_of` target is never itself an alias of a
/// third name (except of itself, the deliberate `-b`/`-b:v` shape), so one
/// hop always reaches the real dispatch key.
fn dispatch_key(e: &Entry) -> &str {
    match &e.alias_of {
        Some(target) if target != &e.name => target.as_str(),
        _ => e.name.as_str(),
    }
}

/// Names inside `refuse_unimplemented_options`'s const arrays — an option
/// listed there is not silently ignored, it fails loudly, which is exactly
/// what "consumed" means for this check's purposes.
fn refused_names(cli_rs: &str) -> Set<String> {
    let Some(start) = cli_rs.find("fn refuse_unimplemented_options") else {
        return Set::new();
    };
    let Some(body) = cli_rs.get(start..) else {
        return Set::new();
    };
    // Stop at the next top-level `\n}\n` after the function starts; good
    // enough for a name-collection sweep, and this function's own closing
    // brace is always at column 0.
    let end = body.find("\n}\n").map_or(body.len(), |e| e + 2);
    let Some(body) = body.get(..end) else {
        return Set::new();
    };
    let mut names = Set::new();
    let mut rest = body;
    while let Some(q1) = rest.find('"') {
        let after = &rest[q1 + 1..];
        let Some(q2) = after.find('"') else { break };
        names.insert(after[..q2].to_string());
        rest = &after[q2 + 1..];
    }
    names
}

fn read_dir_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            read_dir_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// One table's unconsumed dispatch keys, plus every alias name that shares
/// each key (for a report a human can act on without re-deriving aliases).
fn unconsumed(table_path: &Path, dispatch_src: &str, refused: &Set<String>) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(table_path) else {
        return Vec::new();
    };
    let entries = parse_cli_attrs(&text);

    let mut by_key: crate::Map<String, Vec<String>> = crate::Map::new();
    for e in &entries {
        by_key
            .entry(dispatch_key(e).to_string())
            .or_default()
            .push(e.name.clone());
    }

    let mut out = Vec::new();
    for (key, names) in &by_key {
        if refused.contains(key) {
            continue;
        }
        // Most dispatch reads `ParsedOption::resolved().0`, an already-split
        // bare name, so `"key"` is the usual literal. A few options
        // (`-v`/`-loglevel`: see `vaco_cli_core::loglevel`) are read
        // straight off raw argv *before* splitting, because the banner and
        // stats decisions have to be made before the option table exists at
        // all -- those literals keep the leading `-`. Accept either.
        let bare = format!("\"{key}\"");
        let dashed = format!("\"-{key}\"");
        if dispatch_src.contains(&bare) || dispatch_src.contains(&dashed) {
            continue;
        }
        let mut names = names.clone();
        names.sort();
        if names.len() == 1 {
            out.push(format!("    {key}"));
        } else {
            out.push(format!("    {key}  (aliases: {})", names.join(", ")));
        }
    }
    out.sort();
    out
}

/// Report every `CliOptionTable` entry whose dispatch key never appears as a
/// string literal in `vaco-cli`/`vaco-probe`'s own source and is not named in
/// `refuse_unimplemented_options`. Prints and always succeeds — see the
/// module doc for why this cannot be a hard gate or a compile error.
pub fn run(_check: bool) -> Task {
    let root = repo_root();

    let cli_rs_path = root.join("crates/app/vaco-cli/src/cli.rs");
    let cli_rs = std::fs::read_to_string(&cli_rs_path)
        .map_err(|e| format!("{}: {e}", cli_rs_path.display()))?;
    let refused = refused_names(&cli_rs);

    let mut dispatch_files = Vec::new();
    read_dir_rs(&root.join("crates/app/vaco-cli/src"), &mut dispatch_files);
    read_dir_rs(&root.join("crates/app/vaco-probe/src"), &mut dispatch_files);
    let mut dispatch_src = String::new();
    for f in &dispatch_files {
        dispatch_src.push_str(&std::fs::read_to_string(f).unwrap_or_default());
        dispatch_src.push('\n');
    }

    let ffmpeg = unconsumed(
        &root.join("crates/app/vaco-cli-core/src/tables/ffmpeg.rs"),
        &dispatch_src,
        &refused,
    );
    let ffprobe = unconsumed(
        &root.join("crates/app/vaco-cli-core/src/tables/ffprobe.rs"),
        &dispatch_src,
        &refused,
    );

    if ffmpeg.is_empty() && ffprobe.is_empty() {
        println!(
            "option-consumption-check: every CliOptionTable dispatch key is either \
             matched somewhere in vaco-cli/vaco-probe or named in \
             refuse_unimplemented_options"
        );
        return Ok(());
    }

    if !ffmpeg.is_empty() {
        println!(
            "option-consumption-check: {} ffmpeg dispatch key(s) parse and match nothing \
             (report, not a gate -- see this module's doc for why):\n{}",
            ffmpeg.len(),
            ffmpeg.join("\n")
        );
    }
    if !ffprobe.is_empty() {
        println!(
            "option-consumption-check: {} ffprobe dispatch key(s) parse and match nothing:\n{}",
            ffprobe.len(),
            ffprobe.join("\n")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_name_and_alias_of_past_nested_parens() {
        let text = r#"
            #[cli(name = "b", alias_of = "b", spec = "v", argname = "bitrate", flags(HAS_ARG, PER_FILE, OUTPUT, VIDEO), kind = Expr, help = "set the video bitrate")]
            B,
            #[cli(name = "ab", alias_of = "b", spec = "a", flags(HAS_ARG, PER_FILE, OUTPUT, AUDIO), help = "set the audio bitrate")]
            Ab,
        "#;
        let entries = parse_cli_attrs(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "b");
        assert_eq!(entries[0].alias_of.as_deref(), Some("b"));
        assert_eq!(entries[1].name, "ab");
        assert_eq!(entries[1].alias_of.as_deref(), Some("b"));
    }

    #[test]
    fn self_alias_resolves_to_its_own_name() {
        let e = Entry {
            name: "b".to_string(),
            alias_of: Some("b".to_string()),
        };
        assert_eq!(dispatch_key(&e), "b");
    }

    #[test]
    fn plain_alias_resolves_to_its_target() {
        let e = Entry {
            name: "vcodec".to_string(),
            alias_of: Some("codec".to_string()),
        };
        assert_eq!(dispatch_key(&e), "codec");
    }

    #[test]
    fn primary_entry_resolves_to_itself() {
        let e = Entry {
            name: "threads".to_string(),
            alias_of: None,
        };
        assert_eq!(dispatch_key(&e), "threads");
    }

    #[test]
    fn refused_names_reads_the_const_array_only() {
        let src = r#"
fn refuse_unimplemented_options(line: &CommandLine) -> Result<(), Diagnostic> {
    const GLOBAL: &[&str] = &[
        "frame_drop_threshold",
        "n",
    ];
    Ok(())
}

fn some_other_fn() {
    let unrelated = "not_refused";
}
"#;
        let names = refused_names(src);
        assert!(names.contains("frame_drop_threshold"));
        assert!(names.contains("n"));
        assert!(!names.contains("not_refused"));
    }

    #[test]
    fn unconsumed_skips_refused_and_dispatched_keys() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-option-consumption-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let table = dir.join("table.rs");
        std::fs::write(
            &table,
            r#"
#[cli(name = "real", flags(HAS_ARG), kind = Str, help = "dispatched for real")]
Real,
#[cli(name = "refused", flags(HAS_ARG), kind = Str, help = "explicitly refused")]
Refused,
#[cli(name = "dead", flags(HAS_ARG), kind = Str, help = "nobody reads this")]
Dead,
"#,
        )
        .expect("write fixture table");

        let dispatch_src = r#"if name == "real" { do_the_thing(); }"#;
        let mut refused = Set::new();
        refused.insert("refused".to_string());

        let out = unconsumed(&table, dispatch_src, &refused);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(out, vec!["    dead".to_string()]);
    }
}
