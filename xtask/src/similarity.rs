//! The CI similarity scan (QA-08, plan 13 §6.4): winnowing fingerprints, so a
//! PR that reproduces text from a corpus its author must never read from can
//! be caught mechanically instead of trusted to the honour system alone.
//!
//! # Algorithm
//!
//! Schleimer, Wilkerson & Aiken, "Winnowing: Local Algorithms for Document
//! Fingerprinting" (SIGMOD 2003) — the technique MOSS is built on.
//! Implemented here from the paper's description (normalise → k-gram → hash →
//! window-minimum), not ported from any existing similarity-detection tool.
//!
//! 1. **Normalise** ([`tokens`]): strip comments and whitespace, canonicalise
//!    every run of identifier characters to a single `Ident` token and every
//!    run of digits to a single `Num` token. This is deliberately
//!    language-agnostic — the whole point is to compare our Rust against a
//!    C corpus (FFmpeg, x264, dav1d, ...), so nothing here assumes a shared
//!    grammar beyond "identifiers, numbers, and everything else".
//! 2. **k-grams and hashing** ([`kgram_hashes`]): every window of `K` tokens
//!    is hashed as one unit (FNV-1a over the canonical token stream — `xtask`
//!    is deliberately dependency-free, so this is not reaching for a crate
//!    where the standard library already gives us a perfectly good non-
//!    cryptographic hash to write by hand).
//! 3. **Winnowing** ([`winnow`]): keep the minimum hash in every window of
//!    `W` consecutive k-gram hashes, breaking ties toward the rightmost
//!    occurrence (the paper's own rule — it minimises the fingerprint count
//!    without losing the detection guarantee). The result is a fingerprint
//!    set with a guaranteed detection threshold of `K + W - 1` tokens: any
//!    shared run of at least that many tokens produces at least one shared
//!    fingerprint.
//! 4. **Index and query** ([`Index`]): a corpus is indexed once (hash →
//!    every place it was seen); a candidate file is fingerprinted the same
//!    way and its fingerprints are looked up in the index.
//! 5. **Report** ([`Finding`]): our file, our line range, the token length of
//!    the match, and a hash of the matched region. **Never the matched
//!    text itself** — see [`Finding`]'s own doc for why that is load-bearing,
//!    not a nicety.
//!
//! # What this is, and what it deliberately is not
//!
//! The production design (plan 13 §6.4) indexes a real local checkout of
//! FFmpeg/x264/x265/libvpx/dav1d/GStreamer/VLC, once, on an isolated CI
//! runner that no developer machine ever touches. Building or fetching that
//! corpus is out of scope here — this environment has no isolated runner and
//! fetching gigabytes of C source just to index it is disproportionate to
//! what a single xtask module should do on its own initiative. What *is*
//! delivered is the whole algorithm, plus [`run`]'s `--against <dir>` escape
//! hatch so a real CI job can point it at a real checkout later: `cargo
//! xtask similarity-scan --against /path/to/corpus`. Run with no `--against`,
//! it reports that plainly rather than either failing or silently doing
//! nothing — the same convention `--check` gates use for "this needs
//! something that is not available yet".
//!
//! One more deliberate simplification: the plan calls for canonicalising
//! integer literals "except those in declared constant tables" so a
//! spec-mandated table transcribed independently by two implementations does
//! not read as copying. This module canonicalises every numeric literal
//! uniformly instead — cross-referencing `provenance/*.toml` table entries
//! from inside a generic two-corpus text-similarity tool would tie this
//! module to this repository's own provenance schema, which is exactly the
//! kind of coupling a CI job callable against an arbitrary corpus directory
//! should not have. The practical effect is a slightly higher false-positive
//! rate on numeric-table-heavy files, which is the direction to err in: the
//! plan's own §6.4 already treats a constant-table hit as "allowlisted by
//! the provenance record", i.e. reviewed and dismissed, not silently passed.

use crate::{Map, Task};
use std::path::{Path, PathBuf};

/// Tokens per k-gram (plan 13 §6.4).
pub const K: usize = 40;
/// Window width for winnowing (plan 13 §6.4).
pub const W: usize = 20;
/// Guaranteed detection threshold, in tokens: any shared run of at least this
/// many tokens produces at least one shared fingerprint.
pub const DETECTION_THRESHOLD: usize = K + W - 1;

/// One normalised token, with the source line it came from (for reporting —
/// the fingerprint hash itself is computed over [`Token::canon`] alone, never
/// the line number).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub canon: String,
    pub line: usize,
}

/// Strip comments (`//...`, `/* ... */`, `#...` — covers Rust and shell/TOML
/// alike, since the corpus side of this scan may be neither) and whitespace,
/// then canonicalise every identifier run to `Ident` and every digit run to
/// `Num`. Everything else (operators, punctuation, keywords — canonicalised
/// identifiers are indistinguishable from real ones here, deliberately: a
/// keyword is just an identifier that happens to be reserved, and the token
/// stream does not need to know the difference to detect a copied structure)
/// survives as its own single-character token.
#[must_use]
pub fn tokens(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = src.char_indices().peekable();
    let mut line = 1usize;
    let bytes = src.as_bytes();

    while let Some(&(i, c)) = chars.peek() {
        if c == '\n' {
            line += 1;
            chars.next();
            continue;
        }
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // Line comments: `//` or `#`.
        if c == '#' || (c == '/' && bytes.get(i + 1) == Some(&b'/')) {
            while let Some(&(_, c2)) = chars.peek() {
                if c2 == '\n' {
                    break;
                }
                chars.next();
            }
            continue;
        }
        // Block comments: `/* ... */`, possibly multi-line.
        if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            chars.next();
            chars.next();
            let mut prev = '\0';
            for (_, c2) in chars.by_ref() {
                if c2 == '\n' {
                    line += 1;
                }
                if prev == '*' && c2 == '/' {
                    break;
                }
                prev = c2;
            }
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            while let Some(&(_, c2)) = chars.peek() {
                if c2.is_alphanumeric() || c2 == '_' {
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Token {
                canon: "Ident".to_owned(),
                line,
            });
            continue;
        }
        if c.is_ascii_digit() {
            while let Some(&(_, c2)) = chars.peek() {
                if c2.is_alphanumeric() || c2 == '.' || c2 == '_' {
                    // Swallows float/hex/suffix noise (`0x1F`, `1.5f32`,
                    // `1_000`) into one Num token — good enough for a
                    // structural-copy detector, not a lexer.
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Token {
                canon: "Num".to_owned(),
                line,
            });
            continue;
        }
        // A single punctuation/operator character, taken as its own token.
        out.push(Token {
            canon: c.to_string(),
            line,
        });
        chars.next();
    }
    out
}

/// FNV-1a, 64-bit. `xtask` is deliberately dependency-free (see this crate's
/// top-level doc), and this is a well-known, easily-verified non-cryptographic
/// hash — nothing here needs cryptographic properties, only a low collision
/// rate over short strings.
#[must_use]
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Hash every `K`-token window, in order. `positions[i]` is the token index
/// the `i`-th k-gram starts at (needed later to map a fingerprint back to a
/// line range).
#[must_use]
pub fn kgram_hashes(toks: &[Token]) -> Vec<(u64, usize)> {
    if toks.len() < K {
        return Vec::new();
    }
    // `xtask` is dependency-free by design (see this crate's top-level doc),
    // so `vaco_limits::Budget::alloc` — the workspace's usual replacement for
    // `Vec::with_capacity` — is not reachable here; a plain `Vec::new()` and
    // amortised growth is the whole cost, on tool-scale inputs.
    let mut out = Vec::new();
    for start in 0..=(toks.len() - K) {
        let mut joined = String::new();
        for t in &toks[start..start + K] {
            joined.push_str(&t.canon);
            joined.push('\u{1}'); // separator unlikely to appear in a token
        }
        out.push((fnv1a(joined.as_bytes()), start));
    }
    out
}

/// Winnow k-gram hashes into a fingerprint set: the minimum hash in every
/// window of `W` consecutive k-grams, ties broken toward the rightmost
/// occurrence, deduplicated against the immediately preceding selection —
/// exactly the paper's algorithm (§3).
#[must_use]
pub fn winnow(kgrams: &[(u64, usize)]) -> Vec<(u64, usize)> {
    if kgrams.is_empty() {
        return Vec::new();
    }
    if kgrams.len() <= W {
        let best = kgrams
            .iter()
            .copied()
            .fold(kgrams[0], |acc, cand| if cand.0 <= acc.0 { cand } else { acc });
        return vec![best];
    }
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for window in kgrams.windows(W) {
        let mut best = window[0];
        for &cand in window.iter().skip(1) {
            if cand.0 <= best.0 {
                best = cand;
            }
        }
        if last != Some(best.1) {
            out.push(best);
            last = Some(best.1);
        }
    }
    out
}

/// A source file's complete fingerprint set, plus what is needed to turn a
/// hash hit back into `(line range, token length)` without re-tokenising.
#[derive(Debug, Clone)]
pub struct Fingerprints {
    /// `(hash, start_token_index)`, in order.
    pub marks: Vec<(u64, usize)>,
    /// Every token's source line, indexed by token position — how a
    /// `start_token_index` becomes a reportable line range.
    pub token_lines: Vec<usize>,
}

#[must_use]
pub fn fingerprint(src: &str) -> Fingerprints {
    let toks = tokens(src);
    let token_lines = toks.iter().map(|t| t.line).collect();
    let marks = winnow(&kgram_hashes(&toks));
    Fingerprints { marks, token_lines }
}

impl Fingerprints {
    /// `(first_line, last_line)` of the k-gram starting at token `start`.
    fn line_range(&self, start: usize) -> (usize, usize) {
        let first = self.token_lines.get(start).copied().unwrap_or(1);
        let last = self
            .token_lines
            .get(start + K - 1)
            .copied()
            .unwrap_or(first);
        (first, last.max(first))
    }
}

/// A corpus indexed once: every fingerprint hash maps to every `(source name,
/// token start)` it was seen at.
#[derive(Debug, Default)]
pub struct Index {
    by_hash: Map<u64, Vec<(String, usize)>>,
}

impl Index {
    #[must_use]
    pub fn new() -> Self {
        Self { by_hash: Map::new() }
    }

    /// Index one corpus file under `name` (a label for reporting — the
    /// corpus's own text is never retained beyond the hashes).
    pub fn add(&mut self, name: &str, src: &str) {
        let fp = fingerprint(src);
        for (hash, start) in fp.marks {
            self.by_hash.entry(hash).or_default().push((name.to_owned(), start));
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

/// One reported match. Deliberately carries no matched text — see the field
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The file that was scanned (ours).
    pub our_file: String,
    /// `(first_line, last_line)` in `our_file`.
    pub our_lines: (usize, usize),
    /// How many tokens matched — always `>= DETECTION_THRESHOLD`.
    pub token_length: usize,
    /// The corpus source name the match was found in. A label, not a path
    /// into the corpus checkout — the report must be shareable without
    /// handing the reader a way to reconstruct the corpus's own text.
    pub matched_in: String,
    /// FNV-1a of the matched k-gram's canonical token stream. Lets two
    /// findings be compared ("is this the same match reported twice") and
    /// lets a human with access to *both* trees verify it by recomputing the
    /// same hash — without the report itself carrying the text that would
    /// make that verification unnecessary and the report itself a leak.
    pub region_hash: u64,
}

/// Query `our_file`'s fingerprints against `index`. One [`Finding`] per
/// fingerprint hit (a file that shares many overlapping windows with the
/// corpus produces several close-together findings, which is intentional —
/// merging adjacent findings is a reporting nicety, not a correctness
/// requirement, and left undone here (`Merge adjacent findings` in
/// `docs/xtask.md` if this is ever wired into real CI)).
#[must_use]
pub fn scan_file(our_file: &str, src: &str, index: &Index) -> Vec<Finding> {
    let fp = fingerprint(src);
    let mut out = Vec::new();
    for (hash, start) in &fp.marks {
        let Some(hits) = index.by_hash.get(hash) else {
            continue;
        };
        let (first, last) = fp.line_range(*start);
        for (corpus_name, _corpus_pos) in hits {
            out.push(Finding {
                our_file: our_file.to_owned(),
                our_lines: (first, last),
                token_length: DETECTION_THRESHOLD,
                matched_in: corpus_name.clone(),
                region_hash: *hash,
            });
        }
    }
    out
}

/// `cargo xtask similarity-scan [--against <corpus-dir>]`.
///
/// With no `--against`, this reports that no corpus is configured and
/// returns success — the isolated-runner-only design (plan 13 §6.4) means
/// "no corpus available" is the expected state on every machine except that
/// runner, not a failure. With `--against`, every file under the given
/// directory is indexed, then every tracked source file under `crates/` is
/// scanned against it and any finding is printed (file, line range, token
/// length, matched corpus name, region hash — never matched text).
pub fn run(args: &[String]) -> Task {
    let against = args
        .iter()
        .position(|a| a == "--against")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let Some(corpus_dir) = against else {
        println!(
            "similarity-scan: no --against <corpus-dir> given; nothing to compare against. \
             This is expected outside the isolated CI runner (plan 13 §6.4) — see this \
             module's doc comment."
        );
        return Ok(());
    };

    let mut index = Index::new();
    for path in walk_text_files(&corpus_dir) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path.strip_prefix(&corpus_dir).unwrap_or(&path).display().to_string();
        index.add(&name, &src);
    }
    if index.is_empty() {
        return Err(format!(
            "similarity-scan: {} contained no readable text files to index",
            corpus_dir.display()
        ));
    }

    let root = crate::repo_root();
    let mut findings = Vec::new();
    for (_layer, _name, path) in crate::crates() {
        for file in crate::rust_files(&path.join("src")) {
            let Ok(src) = std::fs::read_to_string(&file) else {
                continue;
            };
            let label = file.strip_prefix(&root).unwrap_or(&file).display().to_string();
            findings.extend(scan_file(&label, &src, &index));
        }
    }

    if findings.is_empty() {
        println!(
            "similarity-scan: {} fingerprints indexed from {}, no matches at or above the \
             {}-token threshold",
            index.len(),
            corpus_dir.display(),
            DETECTION_THRESHOLD
        );
        return Ok(());
    }

    for f in &findings {
        println!(
            "similarity-scan: {} lines {}-{}: {} tokens match {} (region {:016x})",
            f.our_file, f.our_lines.0, f.our_lines.1, f.token_length, f.matched_in, f.region_hash
        );
    }
    Err(format!(
        "{} finding(s) at or above the {}-token detection threshold — see plan 13 §6.4 for what \
         a hit means and how it is resolved (a provenance-allowlisted table match is not a \
         failure; anything else goes to the gatekeeper)",
        findings.len(),
        DETECTION_THRESHOLD
    ))
}

fn walk_text_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture here is invented for this test, never copied from any
    /// real codebase — the whole point of this scan is to catch that, so its
    /// own tests cannot be an instance of it.
    const SNIPPET_A: &str = r"
        fn frobnicate(width: u32, height: u32, stride: u32) -> u32 {
            let mut total = 0;
            for row in 0..height {
                for col in 0..width {
                    total = total + row * stride + col;
                }
            }
            total
        }
    ";

    #[test]
    fn identical_files_share_every_fingerprint() {
        let idx_src = SNIPPET_A;
        let mut index = Index::new();
        index.add("corpus_file", idx_src);
        let findings = scan_file("our_file", SNIPPET_A, &index);
        assert!(
            !findings.is_empty(),
            "an exact copy above the detection threshold must be found"
        );
        assert!(findings.iter().all(|f| f.matched_in == "corpus_file"));
    }

    #[test]
    fn renamed_identifiers_and_reformatting_still_match() {
        let renamed = r"
            fn quux(w: u32, h: u32, s: u32) -> u32 {
                let mut acc = 0;
                for y in 0..h {
                    for x in 0..w { acc = acc + y * s + x; }
                }
                acc
            }
        ";
        let mut index = Index::new();
        index.add("corpus_file", SNIPPET_A);
        let findings = scan_file("our_file", renamed, &index);
        assert!(
            !findings.is_empty(),
            "identifier canonicalisation must see through a rename + reformat"
        );
    }

    #[test]
    fn unrelated_code_does_not_match() {
        let unrelated = r"
            struct Point3 { x: f64, y: f64, z: f64 }
            impl Point3 {
                fn dot(&self, other: &Point3) -> f64 {
                    self.x * other.x + self.y * other.y + self.z * other.z
                }
            }
        ";
        let mut index = Index::new();
        index.add("corpus_file", SNIPPET_A);
        let findings = scan_file("our_file", unrelated, &index);
        assert!(findings.is_empty(), "unrelated code must not be reported");
    }

    #[test]
    fn a_run_shorter_than_the_threshold_is_not_reported() {
        // Two files that share only a handful of tokens — well under
        // K + W - 1 — must produce nothing, by the algorithm's own
        // guarantee (a shared run below the threshold is *allowed* to be
        // missed; this asserts the converse never happens for a run this
        // short in practice, which is the useful direction to test).
        let short_a = "fn tiny() -> u32 { 1 + 2 }";
        let short_b = "fn other() -> u32 { 1 + 2 }";
        let mut index = Index::new();
        index.add("corpus_file", short_a);
        let findings = scan_file("our_file", short_b, &index);
        assert!(findings.is_empty());
    }

    #[test]
    fn findings_never_carry_the_matched_text() {
        let mut index = Index::new();
        index.add("corpus_file", SNIPPET_A);
        let findings = scan_file("our_file", SNIPPET_A, &index);
        assert!(!findings.is_empty());
        for f in &findings {
            // The only strings a Finding carries are the file label and the
            // corpus label — assert neither is, or contains, source text by
            // construction (field-level: there is no field capable of
            // holding it). This test exists so a future field addition to
            // `Finding` trips it rather than silently starting to leak text.
            assert_eq!(
                std::mem::size_of_val(f),
                std::mem::size_of::<String>() * 2 + std::mem::size_of::<(usize, usize)>() +
                    std::mem::size_of::<usize>() + std::mem::size_of::<u64>()
            );
        }
    }

    #[test]
    fn tokens_collapse_identifiers_and_numbers() {
        let toks = tokens("let x123 = 0xFF + foo_bar;");
        let canon: Vec<&str> = toks.iter().map(|t| t.canon.as_str()).collect();
        assert_eq!(canon, ["Ident", "Ident", "=", "Num", "+", "Ident", ";"]);
    }

    #[test]
    fn comments_are_stripped_entirely() {
        let toks = tokens("// a comment\nlet x = 1; /* block\ncomment */ let y = 2;");
        let canon: Vec<&str> = toks.iter().map(|t| t.canon.as_str()).collect();
        assert_eq!(
            canon,
            ["Ident", "Ident", "=", "Num", ";", "Ident", "Ident", "=", "Num", ";"]
        );
    }

    #[test]
    fn winnow_selects_rightmost_minimum_on_ties() {
        // Longer than W so the sliding-window branch runs rather than the
        // whole-slice shortcut for short inputs. Two positions (5 and 10)
        // tie for the lowest hash; every other position is strictly larger
        // and distinct, so the only interesting question is which of the
        // tied pair a window that contains both ever reports.
        let mut kgrams: Vec<(u64, usize)> = (0..(W + 2)).map(|i| (100 + i as u64, i)).collect();
        kgrams[5] = (1, 5);
        kgrams[10] = (1, 10);
        let selected = winnow(&kgrams);
        assert!(
            selected.contains(&(1, 10)),
            "the rightmost of a tied pair must be selected: {selected:?}"
        );
        assert!(
            !selected.contains(&(1, 5)),
            "the leftmost of a tied pair must never win once its rightmost twin is in the same \
             window: {selected:?}"
        );
    }

    #[test]
    fn detection_threshold_matches_the_paper() {
        assert_eq!(DETECTION_THRESHOLD, K + W - 1);
    }
}
