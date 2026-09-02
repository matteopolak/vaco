//! Exhaustive pairwise prefix-conflict scan over hand-transcribed VLC/Huffman
//! tables, workspace-wide.
//!
//! # Why this exists, and why it duplicates `vaco-codec-vlc::is_prefix_free`
//! rather than depending on it
//!
//! `vaco-codec-h264`'s CAVLC tables were transcribed from recollection, self
//! -checked (prefix-free, exact code lengths), and still had real errors: 7
//! of 15 `TOTAL_ZEROS_4X4` rows conflicted outright, and — worse — several
//! `COEFF_TOKEN_NC2` rows and over half of `RUN_BEFORE`'s highest-risk row
//! were *wrong but still prefix-free*, so the self-consistency check that
//! caught the first class of error was silent on the second. Prefix-freedom
//! is the weakest of three verification tiers; prefix-freedom plus exact
//! code lengths is stronger but still not sufficient; only line-by-line
//! comparison against primary spec text is. This scan runs the *weakest*
//! tier deliberately — cheap (a pairwise comparison over already-loaded
//! static data, sub-second) and worth running everywhere these tables
//! exist — but is not a substitute for the primary-text pass H.264's own
//! tables were re-verified against, and this module's report says so on
//! every line, not just once at the top.
//!
//! `vaco-codec-vlc::is_prefix_free` already implements the same algorithm,
//! but `xtask` is deliberately dependency-free — this binary gates the
//! build, so it must compile before anything else and must not itself be
//! able to violate the policies it enforces. Depending on a workspace crate
//! here would mean a transient compile break in that crate (not rare in a
//! shared tree with several concurrent writers) takes every gate down with
//! it. The ~15-line algorithm below is re-derived, not copy-pasted.
//!
//! # What this scan covers, and what it structurally cannot
//!
//! Only tables shaped as a flat `(bit-string, ...)` collection. VP8's and
//! VP9's coefficient/mode tables are **not** in scope: they are binary-tree
//! traversal tables (`&[i8]` branch-index arrays) and probability tables,
//! not independently-transcribed bit-strings that could collide with each
//! other — "prefix conflict" is not a meaningful property of a tree encoded
//! that way. AC-3 has no VLC tables at all (mantissa decoding is
//! fixed-width grouped-radix, not variable-length). Both are recorded as
//! "not applicable" below rather than silently skipped.
//!
//! # Ownership
//!
//! This module only ever *reads* the crates it scans. Where a target crate
//! has a live writer (checked via `git status --porcelain` immediately
//! before this module's own targets list was finalised), any finding is
//! reported and left for that crate's owner; nothing here ever edits
//! another crate's table file.

use crate::{Task, repo_root};

/// One VLC-shaped table to scan: which file, which `const` name, and how to
/// pull `(bit_length, code)` pairs out of its source text.
struct Target {
    crate_name: &'static str,
    file: &'static str,
    table: &'static str,
    shape: Shape,
}

/// How a table's entries spell out their own bit pattern in source.
#[derive(Clone, Copy)]
enum Shape {
    /// A struct literal carrying explicit `len:`/`code:` fields
    /// (`vaco-codec-mpegaudio`'s `HuffEntry`).
    LenCodeFields,
    /// A leading `"01..."` string literal that *is* the codeword — either
    /// bare in a tuple (`("01", ...)`) or as a macro's first argument
    /// (`rl!("01", ...)`). Both are one regex: the first quoted run of `0`s
    /// and `1`s after the table's own opening bracket.
    BitString,
}

/// One codeword this scan would otherwise flag every conflict of, that is
/// not a bug — mirrors `dup_check.rs`'s `DISTINCT` allowlist convention:
/// naming one is a claim that it was checked and is intentional, not a way
/// to silence noise. Exempts the *entry*, not a single pair, because an
/// entry a decoder peeks for and special-cases out-of-band (rather than
/// running through the table's own ordinary lookup) is by construction a
/// prefix of more than one of that table's real codewords, and every such
/// pairing is equally intentional.
struct KnownIntentional {
    table: &'static str,
    spelling: &'static str,
    reason: &'static str,
}

const KNOWN_INTENTIONAL: &[KnownIntentional] = &[KnownIntentional {
    table: "TABLE_ZERO",
    spelling: "\"1\"",
    reason: "RunLevel::first_coefficient_only's own doc comment: the lone \"1\" row \
             is valid only as a block's first coefficient, is peeked for and handled \
             separately (block::decode_coefficients) before the ordinary table lookup \
             ever runs, and is documented as deliberately not participating in the \
             normal table's prefix-free property (\"a lone leading 1 bit is never a \
             valid prefix of any other row, so this is unambiguous\") — it is a real \
             prefix of every other short codeword in the table by design, not by error.",
}];

fn is_known_intentional(table: &str, a: &str, b: &str) -> bool {
    KNOWN_INTENTIONAL
        .iter()
        .any(|k| k.table == table && (k.spelling == a || k.spelling == b))
}

const TARGETS: &[Target] = &[
    Target {
        crate_name: "vaco-codec-mpegaudio",
        file: "crates/codec/vaco-codec-mpegaudio/src/huffman_data.rs",
        table: "HUFF_TABLE_1",
        shape: Shape::LenCodeFields,
    },
    Target {
        crate_name: "vaco-codec-mpegaudio",
        file: "crates/codec/vaco-codec-mpegaudio/src/huffman_data.rs",
        table: "HUFF_TABLE_2",
        shape: Shape::LenCodeFields,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "TABLE_ZERO",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "TABLE_ONE",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "MACROBLOCK_ADDRESS_INCREMENT",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "CODED_BLOCK_PATTERN",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "MOTION_CODE",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "DMVECTOR",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "DCT_DC_SIZE_LUMA",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-mpeg12",
        file: "crates/codec/vaco-codec-mpeg12/src/tables.rs",
        table: "DCT_DC_SIZE_CHROMA",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H261_MBA",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H261_MVD",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H261_CBP",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H263_MCBPC_INTRA",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H263_MCBPC_INTER",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H263_CBPY_INTRA",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H263_CBPY_INTER",
        shape: Shape::BitString,
    },
    Target {
        crate_name: "vaco-codec-h263",
        file: "crates/codec/vaco-codec-h263/src/tables.rs",
        table: "H263_MVD",
        shape: Shape::BitString,
    },
];

/// One extracted codeword, keyed by its own literal spelling for reporting.
struct Code {
    len: u8,
    code: u32,
    spelling: String,
}

/// Pull `NAME`'s array body out of `text` — from the `= &[` after `const
/// NAME` up to the matching top-level `];`. Not brace/bracket-depth aware
/// beyond that one pair, which is enough for every target here (none nests
/// another `&[...]` array literal inside an entry).
fn extract_block<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("const {name}");
    let start = text.find(&marker)?;
    // `open` sits just past the array literal's own opening `[` (depth 1
    // already, so the loop below closes on the matching `]` regardless of
    // whether the declaration spans one line (`DMVECTOR`) or many
    // (`TABLE_ZERO`) — a "look for the next `\n];`" heuristic (this
    // module's first draft) silently swallows a single-line table's true
    // end and keeps reading into whatever table follows it, which is a
    // false-positive source distinct from anything a real conflict would
    // produce, so it is worth getting right rather than working around.
    let open = text[start..].find("= &[")? + start + "= &[".len();
    let mut depth: i32 = 1;
    let bytes = text.as_bytes();
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn extract_codes(block: &str, shape: Shape) -> Vec<Code> {
    let mut out = Vec::new();
    match shape {
        Shape::LenCodeFields => {
            let mut rest = block;
            while let Some(len_at) = rest.find("len:") {
                let after_len = &rest[len_at + "len:".len()..];
                let Some(len) = leading_number(after_len) else {
                    rest = after_len;
                    continue;
                };
                let Some(code_at) = after_len.find("code:") else {
                    break;
                };
                let after_code = &after_len[code_at + "code:".len()..];
                let Some(code) = leading_number(after_code) else {
                    rest = after_code;
                    continue;
                };
                out.push(Code {
                    len: len as u8,
                    code,
                    spelling: format!("len:{len},code:{code}"),
                });
                rest = after_code;
            }
        }
        Shape::BitString => {
            let mut rest = block;
            while let Some(quote_at) = rest.find('"') {
                let after_quote = &rest[quote_at + 1..];
                let Some(end) = after_quote.find('"') else {
                    break;
                };
                let literal = &after_quote[..end];
                rest = &after_quote[end + 1..];
                if literal.is_empty() || !literal.bytes().all(|b| b == b'0' || b == b'1') {
                    continue;
                }
                let len = literal.len() as u8;
                let code = u32::from_str_radix(literal, 2).unwrap_or(0);
                out.push(Code {
                    len,
                    code,
                    spelling: format!("\"{literal}\""),
                });
            }
        }
    }
    out
}

fn leading_number(s: &str) -> Option<u32> {
    let s = s.trim_start();
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// The same structural property `vaco-codec-vlc::is_prefix_free` checks —
/// re-derived here rather than shared, per this module's own doc. Returns
/// every conflicting pair found, not just the first, so one run reports
/// everything wrong with a table.
fn prefix_conflicts(codes: &[Code]) -> Vec<(usize, usize)> {
    let mut conflicts = Vec::new();
    for i in 0..codes.len() {
        for j in (i + 1)..codes.len() {
            let (a, b) = (&codes[i], &codes[j]);
            let (short, long) = if a.len <= b.len { (a, b) } else { (b, a) };
            if short.len == 0 || long.len == 0 {
                continue;
            }
            if short.len == long.len {
                if short.code == long.code {
                    conflicts.push((i, j));
                }
                continue;
            }
            let shift = long.len - short.len;
            if (long.code >> shift) == short.code {
                conflicts.push((i, j));
            }
        }
    }
    conflicts
}

pub fn run(_check: bool) -> Task {
    let root = repo_root();
    let mut any_conflict = false;
    let mut report = Vec::new();

    for target in TARGETS {
        let path = root.join(target.file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            report.push(format!(
                "{}::{}: could not read {} (crate moved or renamed?)",
                target.crate_name, target.table, target.file
            ));
            continue;
        };
        let Some(block) = extract_block(&text, target.table) else {
            report.push(format!(
                "{}::{}: `const {}` not found in {} (table renamed?)",
                target.crate_name, target.table, target.table, target.file
            ));
            continue;
        };
        let codes = extract_codes(block, target.shape);
        if codes.is_empty() {
            report.push(format!(
                "{}::{}: extracted zero codewords — shape assumption is stale, check by hand",
                target.crate_name, target.table
            ));
            continue;
        }
        let conflicts = prefix_conflicts(&codes);
        let (allowed, real): (Vec<_>, Vec<_>) = conflicts.into_iter().partition(|(i, j)| {
            is_known_intentional(target.table, &codes[*i].spelling, &codes[*j].spelling)
        });
        if real.is_empty() && allowed.is_empty() {
            report.push(format!(
                "{}::{}: {} codewords, prefix-free (weakest tier only — not checked \
                 against primary text)",
                target.crate_name,
                target.table,
                codes.len()
            ));
        } else {
            if !real.is_empty() {
                any_conflict = true;
            }
            report.push(format!(
                "{}::{}: {} codewords, {} PREFIX CONFLICT(S), {} known-intentional \
                 (see KNOWN_INTENTIONAL):",
                target.crate_name,
                target.table,
                codes.len(),
                real.len(),
                allowed.len()
            ));
            for (i, j) in &real {
                let a = &codes[*i];
                let b = &codes[*j];
                report.push(format!(
                    "    {} (len {}) conflicts with {} (len {})",
                    a.spelling, a.len, b.spelling, b.len
                ));
            }
            for (i, j) in &allowed {
                let a = &codes[*i];
                let b = &codes[*j];
                let reason = KNOWN_INTENTIONAL
                    .iter()
                    .find(|k| k.table == target.table && (k.spelling == a.spelling || k.spelling == b.spelling))
                    .map_or("(reason not found)", |k| k.reason);
                report.push(format!(
                    "    (known-intentional) {} (len {}) / {} (len {}) — {reason}",
                    a.spelling, a.len, b.spelling, b.len
                ));
            }
        }
    }

    println!("vlc-scan: {} table(s) checked across {} crate(s)", TARGETS.len(), {
        let mut names: Vec<&str> = TARGETS.iter().map(|t| t.crate_name).collect();
        names.sort_unstable();
        names.dedup();
        names.len()
    });
    for line in &report {
        println!("  {line}");
    }
    println!(
        "vlc-scan note: a clean result above means prefix-free only (tier 1 of 3 — see \
         AGENT-CONSTRAINTS.md's \"How confident should a transcribed table be\" section). \
         It does not mean the table is correct: a transposed pair of equal-length codes, \
         or any wrong value that happens to still be prefix-free, passes this scan \
         silently — vaco-codec-h264's own CAVLC tables did, in several rows, before being \
         checked against primary text directly."
    );

    if any_conflict {
        Err("vlc-scan: one or more tables have real prefix conflicts — see above".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_block_finds_a_simple_array() {
        let text = "pub const FOO: &[u8] = &[\n1, 2, 3,\n];\nmore text";
        let block = extract_block(text, "FOO").unwrap();
        assert!(block.contains("1, 2, 3"));
    }

    #[test]
    fn len_code_fields_are_extracted_in_order() {
        let block = "HuffEntry { len: 1, code: 1, x: 0, y: 0 },\n\
                      HuffEntry { len: 3, code: 2, x: 0, y: 1 },";
        let codes = extract_codes(block, Shape::LenCodeFields);
        assert_eq!(codes.len(), 2);
        assert_eq!((codes[0].len, codes[0].code), (1, 1));
        assert_eq!((codes[1].len, codes[1].code), (3, 2));
    }

    #[test]
    fn bit_strings_are_extracted_from_bare_tuples_and_macro_calls() {
        let block = "(\"01\", 5),\nrl!(\"101\", 0, 1),\nrl!(\"11\", eob),";
        let codes = extract_codes(block, Shape::BitString);
        assert_eq!(codes.len(), 3);
        assert_eq!((codes[0].len, codes[0].code), (2, 0b01));
        assert_eq!((codes[1].len, codes[1].code), (3, 0b101));
        assert_eq!((codes[2].len, codes[2].code), (2, 0b11));
    }

    #[test]
    fn a_prefix_conflict_is_found_regardless_of_which_entry_is_shorter() {
        let codes = vec![
            Code { len: 1, code: 0b0, spelling: "\"0\"".into() },
            Code { len: 2, code: 0b01, spelling: "\"01\"".into() },
        ];
        let conflicts = prefix_conflicts(&codes);
        assert_eq!(conflicts, vec![(0, 1)]);
    }

    #[test]
    fn equal_length_duplicates_are_a_conflict() {
        let codes = vec![
            Code { len: 3, code: 0b010, spelling: "a".into() },
            Code { len: 3, code: 0b010, spelling: "b".into() },
        ];
        assert_eq!(prefix_conflicts(&codes).len(), 1);
    }

    #[test]
    fn a_clean_code_has_no_conflicts() {
        let codes = vec![
            Code { len: 1, code: 0b0, spelling: "a".into() },
            Code { len: 2, code: 0b10, spelling: "b".into() },
            Code { len: 2, code: 0b11, spelling: "c".into() },
        ];
        assert!(prefix_conflicts(&codes).is_empty());
    }

    #[test]
    fn every_registered_target_file_and_table_actually_exists_and_extracts() {
        let root = repo_root();
        for target in TARGETS {
            let path = root.join(target.file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", target.file));
            let block = extract_block(&text, target.table)
                .unwrap_or_else(|| panic!("{}: const {} not found", target.file, target.table));
            let codes = extract_codes(block, target.shape);
            assert!(
                !codes.is_empty(),
                "{}::{}: extracted zero codewords",
                target.crate_name,
                target.table
            );
        }
    }
}
