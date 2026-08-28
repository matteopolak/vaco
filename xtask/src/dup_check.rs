//! One definition per concept (D19).
//!
//! # Why this is a gate and not an audit
//!
//! A one-off search under-reports. The first pass over this workspace missed
//! `vaco_format_core::Disposition` entirely, because it is declared inside a
//! `bitflags!` invocation and so is indented — a `^pub struct` pattern never
//! sees it. It missed the one type already known to be duplicated, which is
//! exactly the failure mode a manual audit has.
//!
//! So the check runs in CI over an explicit allowlist. A new duplicate name
//! fails the build and has to be either merged or justified in writing.
//!
//! # What it can and cannot tell you
//!
//! It compares **names**, which is a proxy. Two crates may share a name and mean
//! different things — `Tier` is an HEVC tier and a SIMD tier; `Component` is a
//! pixel component and a registry component. Those go in [`DISTINCT`] with the
//! reason, and the reason is the point: writing it down is what stops the list
//! becoming a place to hide real duplication.
//!
//! It cannot see two types that mean the same thing under different names. That
//! needs a person.

use crate::{Map, Task, crates};

/// Names that legitimately appear in more than one crate, and why.
///
/// Adding a row is a claim that the two are *different concepts*. If they are
/// the same concept, merge them instead — that is what D19 asks for.
const DISTINCT: &[(&str, &str)] = &[
    (
        "Caps",
        "vaco-simd: CPU features. vaco-demux-matroska: track capabilities.",
    ),
    (
        "Chain",
        "vaco-conformance: a comparison chain. vaco-filter-graph: a filter chain.",
    ),
    (
        "Channel",
        "vaco-chlayout: an audio channel. vaco-conformance: a reporting channel.",
    ),
    (
        "State",
        "vaco-filter-adsp: a biquad's Direct Form I delay line (x1/x2/y1/y2). \
         vaco-protocol-http: reconnect attempt counting. Same word, no shared \
         concept — one is four f64 samples of filter memory, the other is a \
         retry budget with a first-failure timestamp.",
    ),
    (
        "Component",
        "vaco-pixfmt: a pixel component. vaco-registry: a registered component.",
    ),
    (
        "Constraint",
        "vaco-filter-core: a format constraint. vaco-parse-hevc: a profile constraint.",
    ),
    ("Counter", "distinct counters in two filter crates."),
    (
        "Direction",
        "vaco-tx: forward/inverse transform. vaco-filter-core: pad direction.",
    ),
    (
        "Discovery",
        "vaco-format-core: stream discovery. vaco-conformance: corpus discovery.",
    ),
    (
        "FilterSpec",
        "vaco-filter-graph: a parsed filter. vaco-scale: a scaler kernel spec.",
    ),
    (
        "Frame",
        "vaco-frame: the frame model. vaco-demux-matroska: a laced block frame.",
    ),
    (
        "Label",
        "vaco-chlayout: a channel label. vaco-filter-graph: a link label.",
    ),
    (
        "Limits",
        "vaco-limits: the resource budget. vaco-expr: expression depth bounds.",
    ),
    ("Mode", "distinct modes in vaco-core and vaco-parse-opus."),
    (
        "Plan",
        "vaco-tx: a transform plan. vaco-scale: a conversion plan.",
    ),
    (
        "Scope",
        "vaco-conformance: a test scope. vaco-probe: an option scope.",
    ),
    (
        "Section",
        "vaco-format-mpegts-tables: a PSI section. vaco-conformance: a report section.",
    ),
    (
        "Signal",
        "vaco-conformance: a test signal. vaco-parse-aac: a signalling field.",
    ),
    (
        "Step",
        "distinct step types in vaco-codec-core and vaco-filter-framesync.",
    ),
    (
        "Tier",
        "vaco-simd: a SIMD tier. vaco-parse-hevc: an HEVC tier. vaco-conformance: a suite tier.",
    ),
    (
        "Timeline",
        "vaco-filter-core: enable= timeline. vaco-format-isom: an ISOBMFF timeline.",
    ),
    (
        "Token",
        "vaco-cli-core: an argv token. vaco-filter-graph: a graph-string token.",
    ),
    (
        "Variant",
        "vaco-format-adaptive: an HLS/DASH bitrate-ladder rung (EXT-X-STREAM-INF / Representation). vaco-mux-matroska: which of Matroska/WebM a muxer instance writes.",
    ),
    (
        "Violation",
        "distinct violation reports in vaco-codec-core and vaco-filter-core.",
    ),
    (
        "Window",
        "vaco-resample: an FIR window. vaco-parse-hevc: a conformance window.",
    ),
    // --- H.264 and HEVC parse the same *kind* of structure with different
    // syntax, so these are genuinely separate types today. `vaco-codec-cbs` is
    // the crate meant to unify what can be unified; until it says which, these
    // stay. See D19's scheduled work.
    (
        "BitstreamRestriction",
        "H.264 and HEVC VUI; different syntax (D19: cbs)",
    ),
    ("ChromaFormat", "H.264 and HEVC (D19: cbs)"),
    ("CpbEntry", "H.264 and HEVC HRD (D19: cbs)"),
    (
        "HrdParameters",
        "H.264 and HEVC HRD; different syntax (D19: cbs)",
    ),
    (
        "NalUnitType",
        "H.264 and HEVC have different NAL type enums (D19: cbs)",
    ),
    (
        "ParameterSets",
        "H.264 and HEVC parameter-set stores (D19: cbs)",
    ),
    ("PicStruct", "H.264 and HEVC SEI pic_struct (D19: cbs)"),
    ("PicStructHint", "H.264 and HEVC (D19: cbs)"),
    ("PictureInfo", "H.264 and HEVC (D19: cbs)"),
    (
        "PictureOrderCount",
        "H.264 and HEVC POC differ structurally (D19: cbs)",
    ),
    ("PocState", "H.264 and HEVC (D19: cbs)"),
    (
        "Pps",
        "H.264 and HEVC picture parameter sets are different structures",
    ),
    ("PredWeightTable", "H.264 and HEVC (D19: cbs)"),
    ("RefPicListModification", "H.264 and HEVC (D19: cbs)"),
    ("SeiMessage", "H.264 and HEVC (D19: cbs)"),
    ("SeiPayload", "H.264 and HEVC (D19: cbs)"),
    (
        "SliceHeader",
        "H.264 and HEVC slice headers are different structures",
    ),
    ("SliceKind", "H.264 and HEVC slice types (D19: cbs)"),
    (
        "Sps",
        "H.264 and HEVC sequence parameter sets are different structures",
    ),
    ("Timing", "H.264 and HEVC VUI timing (D19: cbs)"),
    (
        "VuiParameters",
        "H.264 and HEVC VUI; different syntax (D19: cbs)",
    ),
    (
        "Crop",
        "vaco-frame: a crop rectangle. vaco-parse-h264: the SPS frame-crop offsets.",
    ),
    (
        "Cursor",
        "vaco-probe: an interval's progress through a packet stream (end bound, \
         packets seen, deadline). vaco-format-nut: a byte-slice reader for NUT's \
         `v`/`s`/`vb` varint decoding. No shared concept, coincidental name.",
    ),
    (
        "StreamHeader",
        "vaco-mux-hash: a print-time pair of (CodecParameters, TimeBase) for the \
         `#stream`/`#tb` lines framehash/framemd5/framecrc write — nothing to do \
         with any wire format, generic across every codec. vaco-format-nut: a \
         literal transcription of NUT's own spec section titled `stream_header` \
         (stream_id, stream_class, fourcc, time_base_id, msb_pts_shift, \
         max_pts_distance, decode_delay, stream_flags, codec_specific_data, plus \
         video/audio-specific fields) — NUT-bitstream-shaped, not a general \
         per-stream record. Checked before writing this row, per the \
         orchestrator's prompt: the two do not overlap even after mux-hash's \
         StreamHeader grew `spec_time_base` for #634 — that field has no NUT-side \
         analogue and NUT's fourcc/decode_delay/stream_flags have no mux-hash-side \
         analogue. Merging would mean grafting an unrelated field onto whichever \
         one people notice least.",
    ),
];

/// Known duplicates that are *not* yet resolved, with the plan.
///
/// Distinct from [`DISTINCT`]: these are the same concept twice, tracked so they
/// cannot be forgotten and cannot grow silently.
const KNOWN_DUPLICATE: &[(&str, &str)] = &[];

// Both original entries are resolved. `CancelToken` and `Disposition` now live
// in `vaco-core`, below every crate that wanted them, and the old spellings are
// re-exports.
//
// Worth recording what merging them found, because it is the argument for D19
// that a "nothing is wrong today" note cannot make:
//
// - The two `CancelToken`s were byte-identical, so a transcode held one I/O
//   token and one decode token and cancelling either left the other running.
//   "Stop" meant whichever half the caller reached for.
// - The two `Disposition`s disagreed about **case**. One matched names
//   case-insensitively and one did not, and the reference is case-sensitive —
//   measured, and it says so: `Undefined constant or missing \'(\' in
//   \'DEFAULT\'`. One duplication was quietly two behaviours, and the
//   case-insensitive half accepted input the reference rejects.
//
// Duplication is not merely wasteful. It is where two behaviours hide behind
// one name, and neither shows up until someone puts them side by side.

pub fn run(_check: bool) -> Task {
    let mut seen: Map<String, Vec<String>> = Map::new();

    for (_layer, name, path) in crates() {
        let src = path.join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for line in text.lines() {
                    // Leading whitespace allowed on purpose: a `bitflags!` body
                    // is indented, and that is where the one known duplicate
                    // hid from the first manual pass.
                    let t = line.trim_start();
                    for kw in ["pub struct ", "pub enum "] {
                        if let Some(rest) = t.strip_prefix(kw) {
                            let ident: String = rest
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            if ident.chars().next().is_some_and(char::is_uppercase) {
                                let e = seen.entry(ident).or_default();
                                if !e.contains(&name) {
                                    e.push(name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut unexplained = Vec::new();
    for (ident, owners) in &seen {
        if owners.len() < 2 {
            continue;
        }
        let known = DISTINCT.iter().any(|(n, _)| n == ident)
            || KNOWN_DUPLICATE.iter().any(|(n, _)| n == ident);
        if !known {
            unexplained.push(format!("  {ident}: {}", owners.join(", ")));
        }
    }

    if !unexplained.is_empty() {
        unexplained.sort();
        return Err(format!(
            "{} type name(s) defined in more than one crate with no recorded \
             reason (D19):\n{}\n\nMerge them, or — if they are genuinely \
             different concepts — add a row to `DISTINCT` in \
             xtask/src/dup_check.rs saying what each one means. Writing the \
             reason down is what stops that list becoming a place to hide real \
             duplication.",
            unexplained.len(),
            unexplained.join("\n")
        ));
    }

    println!(
        "dup-check: {} shared names, all accounted for ({} distinct by design, \
         {} known duplicates tracked)",
        DISTINCT.len() + KNOWN_DUPLICATE.len(),
        DISTINCT.len(),
        KNOWN_DUPLICATE.len()
    );
    for (name, plan) in KNOWN_DUPLICATE {
        println!(
            "  outstanding: {name} — {}",
            plan.split('.').next().unwrap_or(plan)
        );
    }
    Ok(())
}
