//! Every registered decoder has a differential decode case, or a reviewed
//! reason it does not (priority task recorded in
//! `planning/CONFORMANCE-FINDINGS.md` #68).
//!
//! # What it is
//!
//! Before this gate, `vaco-conformance`'s 709 cases compared probe metadata
//! and remux structure only -- never a decoded pixel or sample. A decoder
//! could be registered, advertised, and completely wrong (FFV1 was wrong on
//! 99.6% of bytes; AC-3 on 99.5% of samples) while every case passed. This
//! closes the *coverage* half of that gap: it does not check that a decode
//! case measures anything correctly (that is `compare::raw`'s job), only
//! that one exists for every decoder this workspace registers.
//!
//! # How it works
//!
//! Reads the same `vaco-component.toml` fragments [`crate::registry`]
//! assembles the real registry from -- the single source of truth for
//! "what decoder is registered," not a hand-maintained list here. A decoder
//! is "covered" when some `tests/conformance/**/*.toml` manifest tags a
//! `[[media]]` entry `"decoder:<name>"`. Anything neither covered nor in
//! [`NOT_YET_COVERED`] fails the gate — the same shape as `owner_gate`'s
//! `MEDIA` and `option_name_gate`'s `KNOWN_GAPS`: a newly registered decoder
//! is covered or explicitly, reviewably deferred, never silently untested.
//!
//! # How to change it
//!
//! Adding a decoder to a crate's `vaco-component.toml` and nothing else now
//! fails this gate. Either tag a `[[media]]` entry for it in a
//! `tests/conformance/transcode/decode-*.toml` manifest with
//! `tags = ["decoder:<name>"]`, or add `(<name>, "<measured reason>")` to
//! [`NOT_YET_COVERED`] — the reason must name what was actually checked
//! (no ffmpeg encoder for it, a text/bitmap subtitle output this harness's
//! pixel/sample modes do not compare, a specific reproduction that blocks a
//! fixture), not "TODO".
//!
//! [`NOT_YET_COVERED`] also has a reverse check: an entry whose decoder
//! *is* covered is stale and fails the gate too, the same reasoning
//! `option_name_gate`'s own doc gives for not letting `KNOWN_GAPS` silently
//! rot in the other direction.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Task, crates, repo_root, tracked_files};

/// Decoders with no decode case yet, and the measured reason. Every entry
/// was checked directly against this machine's own ffmpeg 9.0.1 and a
/// full-feature `vaco` build on 2026-09-02 — see
/// `planning/CONFORMANCE-FINDINGS.md` #68 for the reproductions.
const NOT_YET_COVERED: &[(&str, &str)] = &[
    // Subtitle decoders: output is text or a bitmap overlay, not a raw
    // pixel/sample stream `raw-exact`/`raw-tolerant` compares directly. A
    // real case for these needs a mode this crate does not have yet
    // (a text/bitmap-aware structured diff) — out of scope for the
    // pixel/sample decode census this pass built.
    ("ass", "text subtitle output; not a pixel/sample stream"),
    ("cc_dec", "text subtitle output; not a pixel/sample stream"),
    ("dvb_teletext", "text subtitle output; not a pixel/sample stream"),
    ("dvbsub", "bitmap subtitle overlay; needs a compare mode this crate does not have yet"),
    ("dvdsub", "bitmap subtitle overlay; needs a compare mode this crate does not have yet"),
    ("mov_text", "text subtitle output; not a pixel/sample stream"),
    ("pgssub", "bitmap subtitle overlay; needs a compare mode this crate does not have yet"),
    ("ssa", "text subtitle output; not a pixel/sample stream"),
    ("subrip", "text subtitle output; not a pixel/sample stream"),
    ("text", "text subtitle output; not a pixel/sample stream"),
    ("ttml", "text subtitle output; not a pixel/sample stream"),
    ("webvtt", "text subtitle output; not a pixel/sample stream"),
    // No local, network-free fixture path found this pass. r10k, r210,
    // v210, y41p and avui were in this list until 2026-09-02's second
    // pass found each a real fixture via MOV instead of a bare rawvideo
    // dump (see the coordinator's own steer: a missing-encoder or
    // demux-blocker deferral is a fixture problem worth attacking, not a
    // fundamental one) -- they are covered now; see
    // decode-video-r10k-r210.toml, decode-video-v210.toml,
    // decode-video-y41p.toml and decode-video-avui.toml.
    ("mp1", "ffmpeg 9.0.1 has no mp1 encoder (`-encoders` confirms mp2/mp2fixed only)"),
    (
        "qoa",
        "ffmpeg 9.0.1 demuxes qoa but has no encoder for it (`-encoders` confirms); the \
         fuzz/corpus/qoa_decode seeds checked 2026-09-02 all carry fuzzer-mutated headers \
         (sample_rate up to 16777215, channels up to 255) with no genuine content to compare \
         against a reference decode",
    ),
    (
        "comfortnoise",
        "ffmpeg's comfortnoise encoder is an RFC 3389 generator, not a content transcode -- \
         there is no reference content to decode and compare; the \
         fuzz/corpus/comfortnoise_parse seeds are bare, headerless payloads for the fuzz \
         target's own entry point, not files any container demuxer recognises",
    ),
    (
        "webp",
        "this ffmpeg 9.0.1 build has no webp encoder (`-encoders` confirms); checked all 41 \
         fuzz/corpus/webp_decode seeds over 200 bytes on 2026-09-02 as an alternate fixture \
         source and every one is a fuzzer-mutated file the reference's own decoder also \
         rejects (\"Decoding error: Invalid data found\") -- not usable as a case comparing \
         against a working reference decode",
    ),
    ("jpegxl", "this ffmpeg 9.0.1 build has neither a jpegxl encoder nor decoder (`-decoders`/`-encoders` confirm) -- no oracle to compare against at all"),
    (
        "theora",
        "this ffmpeg 9.0.1 build has no theora encoder (`-encoders` confirms); \
         fuzz/corpus/theora_decode holds bare per-packet payloads for the fuzz target's own \
         entry point (checked 2026-09-02), not a file any Ogg-aware demuxer opens",
    ),
    (
        "vc1",
        "ffmpeg has no vc1/wmv3 encoder (proprietary; `-encoders` confirms decode-only); \
         fuzz/corpus/vc1_decode holds bare per-frame payloads for the fuzz target's own entry \
         point (checked 2026-09-02), not a file any container demuxer opens",
    ),
    ("v210x", "ffmpeg has no v210x encoder (`-encoders` confirms decode-only)"),
    (
        "wrapped_avframe",
        "internal AVFrame passthrough pseudo-codec; checked 2026-09-02 that no muxer (mov, \
         nut, rawvideo, avi) accepts a codec tag for it either -- no file format stores it",
    ),
    (
        "bitpacked",
        "checked 2026-09-02 against mov as well as rawvideo: mov's own muxer refuses \
         (\"Could not find tag for codec bitpacked\") and the rawvideo round trip the \
         reference decoder itself rejects (\"Invalid data found when processing input\") -- \
         no container this pass tried stores it",
    ),
];

fn decoder_components(root: &std::path::Path) -> Result<BTreeMap<String, String>, String> {
    let mut decoders = BTreeMap::new();
    for (_layer, name, path) in crates() {
        let frag = path.join("vaco-component.toml");
        if !frag.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&frag).map_err(|e| format!("{name}: {e}"))?;
        for t in crate::toml::tables(&text, &["component"]).map_err(|e| format!("{}: {e}", frag.display()))? {
            if t.get("kind") == Some("decoder")
                && let Some(cname) = t.get("name")
            {
                decoders.insert(cname.to_owned(), name.clone());
            }
        }
    }
    let _ = root;
    Ok(decoders)
}

/// `"decoder:<name>"` tags found in any tracked `tests/conformance/**/*.toml`
/// manifest's `[[media]]` blocks.
///
/// This does not use [`crate::toml`] on the manifest files themselves —
/// `vaco-conformance`'s own manifest schema (nested tables, inline tables,
/// arrays of strings) is richer than the flat `key = "string"` subset that
/// module reads, and building a second manifest parser here just to read one
/// array field would be the "one dialect beats two" reasoning this repo
/// already applies, backwards. A `"decoder:name"` tag is a string literal
/// inside a TOML array either way, so a direct substring scan is exact for
/// the one thing this gate needs and cannot be fooled by the surrounding
/// schema.
/// Every `"decoder:<name>"` string literal in `text`, wherever it appears.
///
/// A `[[media]]` block's `tags = [...]` array is the only place this repo's
/// manifests put one, but the scan does not need to know that — see this
/// module's own doc for why a direct substring scan, not a second manifest
/// parser, is the right tool.
fn extract_decoder_tags(text: &str) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    let mut rest = text;
    while let Some(pos) = rest.find("\"decoder:") {
        let after = &rest[pos + "\"decoder:".len()..];
        let Some(end) = after.find('"') else { break };
        tags.insert(after[..end].to_owned());
        rest = &after[end..];
    }
    tags
}

fn tagged_decoders(root: &std::path::Path) -> Result<BTreeSet<String>, String> {
    let mut tagged = BTreeSet::new();
    let Some(tracked) = tracked_files() else {
        return Ok(tagged);
    };
    let conformance_dir = root.join("tests").join("conformance");
    for path in tracked {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        if !path.starts_with(&conformance_dir) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        tagged.extend(extract_decoder_tags(&text));
    }
    Ok(tagged)
}

pub fn run(_check: bool) -> Task {
    let root = repo_root();
    let decoders = decoder_components(&root)?;
    let tagged = tagged_decoders(&root)?;
    let not_yet_covered: BTreeMap<&str, &str> = NOT_YET_COVERED.iter().copied().collect();

    if not_yet_covered.len() != NOT_YET_COVERED.len() {
        return Err("NOT_YET_COVERED has a duplicate decoder name".to_owned());
    }

    let mut missing = Vec::new();
    let mut stale_allowlist = Vec::new();
    for (name, owner) in &decoders {
        let covered = tagged.contains(name);
        let deferred = not_yet_covered.contains_key(name.as_str());
        if !covered && !deferred {
            missing.push(format!("  {name} ({owner}): no decode case and not in NOT_YET_COVERED"));
        }
        if covered && deferred {
            stale_allowlist.push(format!(
                "  {name}: has a decode case now but is still listed in NOT_YET_COVERED -- remove the entry"
            ));
        }
    }
    for name in not_yet_covered.keys() {
        if !decoders.contains_key(*name) {
            stale_allowlist.push(format!(
                "  {name}: in NOT_YET_COVERED but no `vaco-component.toml` registers a decoder by that name any more"
            ));
        }
    }

    if !missing.is_empty() || !stale_allowlist.is_empty() {
        let mut msg = String::new();
        if !missing.is_empty() {
            missing.sort();
            msg.push_str(&format!(
                "{} registered decoder(s) have no decode case and no reviewed reason:\n{}\n\n\
                 Tag a `[[media]]` entry `tags = [\"decoder:<name>\"]` in a \
                 tests/conformance/transcode/decode-*.toml manifest, or add \
                 `(\"<name>\", \"<measured reason>\")` to NOT_YET_COVERED in \
                 xtask/src/decoder_coverage.rs.\n\n",
                missing.len(),
                missing.join("\n")
            ));
        }
        if !stale_allowlist.is_empty() {
            stale_allowlist.sort();
            msg.push_str(&format!(
                "{} NOT_YET_COVERED entr(y/ies) are stale:\n{}\n",
                stale_allowlist.len(),
                stale_allowlist.join("\n")
            ));
        }
        return Err(msg);
    }

    let covered_count = decoders.len() - not_yet_covered.len();
    println!(
        "decoder-coverage: {} registered decoder(s), {covered_count} with a decode case, \
         {} deferred with a reviewed reason",
        decoders.len(),
        not_yet_covered.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_coverage_is_clean_against_the_real_tree() {
        // Same shape as reachability_check's own
        // `check_bsf_reachable_is_clean_against_the_real_tree`: not a
        // specific count (that would break on every new decoder or new
        // manifest), just clean against today's NOT_YET_COVERED.
        run(false).expect("every registered decoder has a decode case or a reviewed reason");
    }

    #[test]
    fn not_yet_covered_has_no_duplicate_name() {
        let mut names: Vec<&str> = NOT_YET_COVERED.iter().map(|(n, _)| *n).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "NOT_YET_COVERED lists the same decoder twice");
    }

    #[test]
    fn a_decoder_tag_is_found_regardless_of_surrounding_toml_shape() {
        let text = "[[media]]\nid = \"x\"\ntags = [\"decoder:made_up_codec\", \"other\"]\n";
        assert_eq!(extract_decoder_tags(text), BTreeSet::from(["made_up_codec".to_owned()]));
    }

    #[test]
    fn two_tags_in_one_file_are_both_found() {
        let text = "tags = [\"decoder:a\"]\n...\ntags = [\"decoder:b\", \"decoder:c\"]\n";
        assert_eq!(
            extract_decoder_tags(text),
            BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
    }

    #[test]
    fn text_with_no_decoder_tag_finds_nothing() {
        assert!(extract_decoder_tags("tags = [\"audio\"]\n").is_empty());
    }
}
