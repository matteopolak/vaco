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
        "FrameHeader",
        "vaco-codec-vp8: RFC 6386 §9's compressed frame header (segmentation, \
         loop filter, quantiser indices, entropy-probability updates) — a \
         decode-time record with persistent state fields (`segmentation`, \
         `lf_deltas`) threaded across frames. vaco-parse-av1: AV1's \
         uncompressed_header() syntax record, a plain parse result with no \
         cross-frame state of its own. vaco-codec-vp9: the VP9 Bitstream & \
         Decoding Process Specification's own uncompressed_header() plus \
         the compressed header's forward-updated entropy tables — a third, \
         again independently-shaped, bitstream's own header record. Three \
         different bitstreams, three different shapes.",
    ),
    (
        "ImageDecoder",
        "vaco-codec-pnm: pbm/pgm/ppm/pam/pfm/phm. vaco-codec-image-simple: \
         bmp/pcx/tga/sgi/xwd/xbm. Same small SendReceive-over-Machine \
         wrapper shape, deliberately duplicated rather than shared, since \
         each wraps a disjoint set of decode functions for a codec crate \
         neither owns.",
    ),
    (
        "ImageEncoder",
        "vaco-codec-pnm and vaco-codec-image-simple, same reason as \
         ImageDecoder above.",
    ),
    (
        "Label",
        "vaco-chlayout: a channel label. vaco-filter-graph: a link label.",
    ),
    (
        "Limits",
        "vaco-limits: the resource budget. vaco-expr: expression depth bounds.",
    ),
    (
        "LinkStats",
        "vaco-filter-core: a filter-graph link's queue counters (frames, \
         samples, peak queue depth, times a push was refused for room) — \
         entirely local, in-process bookkeeping about one pad-to-pad \
         connection. vaco-protocol-rist: RIST bonding's per-network-link \
         receive counter (packets_received, keyed by link_id in a \
         BondedReceiver) — RFC-adjacent (TR-06-1/-2 §5.4/§5.5) statistics \
         about a physical/tunnel path's own traffic. No shared concept: one \
         counts frames crossing an in-memory queue, the other counts \
         packets arriving on a network link.",
    ),
    ("Mode", "distinct modes in vaco-core and vaco-parse-opus."),
    (
        "Origin",
        "vaco-format-rtp: an SDP `o=` line's session-originator metadata \
         (username/session-id/network-type/address). vaco-protocol-rist: \
         RIST TR-06-1 \u{a7}5.3.3's SSRC-LSB tag distinguishing an original \
         packet from its retransmission. No shared concept.",
    ),
    (
        "Picture",
        "vaco-codec-vp8: a decoded frame's three reconstruction planes \
         (Y/U/V), held in a reference-frame slot for later inter prediction. \
         vaco-format-id3: an APIC/PIC attached-picture frame's decoded \
         metadata (mime type, picture type, description). No shared concept.",
    ),
    (
        "Plan",
        "vaco-tx: a transform plan. vaco-scale: a conversion plan.",
    ),
    (
        "Plane",
        "vaco-frame: the pool-backed, budget-allocated plane view every \
         other crate reads/writes video pixels through. vaco-codec-vp8: a \
         private, plain `Vec<u8>` reconstruction buffer used only inside \
         this crate's own decode loop, where intra prediction and the loop \
         filter need to read already-written pixels of the same buffer \
         being written -- see the crate's `framebuf` module doc for why \
         `vaco_frame::Plane`'s borrow shape does not fit that access \
         pattern. Copied into a real `vaco_frame::Frame` once, at emission.",
    ),
    (
        "Scope",
        "vaco-conformance: a test scope. vaco-probe: an option scope.",
    ),
    (
        "Segment",
        "vaco-format-imf: one `<Segment>` of a Composition Playlist (SMPTE \
         ST 2067-3) — an ordered set of `Sequence`s forming part of the \
         composition's edit-decision-list timeline. vaco-format-isom: a \
         resolved edit-list entry (ISO/IEC 14496-12 `elst`), entirely in \
         media-timescale ticks — a different container family's \
         independently-named concept, not a shared one.",
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
    (
        "Cell",
        "vaco-codec-subtitle-teletext: one 40x25 page-grid character cell \
         (glyph plus Table 26 spacing attributes). vaco-codec-subtitle-cc: a \
         closed-caption cell. Same shape, unrelated wire formats and control \
         codes (EN 300 706 vs CEA-608/708).",
    ),
    (
        "Color",
        "vaco-codec-subtitle-teletext: one of Teletext's eight fixed CLUT-0 \
         colours. vaco-codec-subtitle-cc: a closed-caption colour.",
    ),
    (
        "ColorConfig",
        "vaco-codec-vp9: VP9's §6.2.2 color_config() — bit depth, color \
         space, range, and chroma subsampling only. vaco-parse-av1: AV1's \
         sequence_header_obu() color_config(), a superset shape adding \
         H.273 color primaries/transfer/matrix and chroma sample position \
         that VP9's syntax has no room for. Different bitstream, different \
         shape.",
    ),
    (
        "EntropyContext",
        "vaco-codec-vp8: RFC 6386 §13's coefficient/motion-vector/mode \
         probability tables (`coeff_probs`, `mv_probs`, `ymode_prob`, \
         `uv_mode_prob`), reset and forward-updated per VP8's own syntax. \
         vaco-codec-vp9: the VP9 spec's independently-shaped probability \
         tables (`coef_probs`, `skip_prob`, `tx_probs`) reset by \
         setup_past_independence() and forward-updated by VP9's own \
         compressed header — a different bitstream's own probability model, \
         not a shared concept.",
    ),
    (
        "Segmentation",
        "vaco-codec-vp8: RFC 6386 §9.3/§10's per-macroblock segmentation \
         (`quant_idx`/`lf_level` deltas or absolutes, keyed by the polarity \
         documented on `absolute`). vaco-codec-vp9: the VP9 spec's \
         independently-shaped §6.2.11 segmentation_params() (its own \
         feature-bit/feature-data layout, `SEG_LVL_*` constants, and \
         persistence-across-frames rules) — a different bitstream's own \
         segmentation syntax, not a shared concept.",
    ),
    (
        "Packet",
        "vaco-packet: a demuxed elementary-stream packet, the type most of \
         this workspace passes around. vaco-codec-subtitle-teletext: one raw \
         42-byte EN 300 706 packet (magazine/packet address plus 40 data \
         bytes) before Hamming/parity decode — never leaves this crate's \
         `packet` module.",
    ),
    (
        "TomlError",
        "vaco-conformance: its own from-scratch TOML reader's parse error \
         (`src/toml.rs`, plan 13 \u00a71.5.1's manifest format). vaco-corpus: an \
         independently written from-scratch TOML reader's parse error \
         (`src/toml_min.rs`) for `vaco-media.lock` and its own suite catalogue. \
         Same *kind* of thing by construction \u2014 both are \"line + message\" \
         errors for a bespoke TOML subset \u2014 but two separate crates, each \
         choosing not to depend on the other's parser rather than pull an \
         unrelated dependency graph (a filter/codec-heavy conformance harness \
         into a corpus-fetching tool, or vice versa). See either module's own \
         docs for why extending either parser further should mean adopting a \
         real TOML crate instead.",
    ),
    (
        "Value",
        "vaco-protocol-rtmp (`amf0::Value`): one decoded AMF0 value from RTMP's \
         wire format (Number/Boolean/String/Object/\u2026), per the AMF0 spec. \
         vaco-conformance (`toml::Value`): one parsed TOML scalar/array/table \
         from that crate's own bespoke reader. Pre-existing, previously \
         unrecorded collision \u2014 recorded here rather than left to keep \
         failing every agent's `dup-check` run.",
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
