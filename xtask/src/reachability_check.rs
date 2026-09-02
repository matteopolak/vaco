//! Components that exist, compile, pass their own tests, and cannot be
//! reached from the CLI.
//!
//! This project has shipped that shape of bug roughly eight times: an H.264
//! decoder callable only from `#[cfg(test)]` code, `vaco-codec-opus` fully
//! implemented and depended on by nothing, FLAC misdetected as CDG because no
//! FLAC demuxer was ever registered, a bitstream-filter family left out of the
//! hand-assembled dispatch list, and more — each found by accident while
//! chasing something else. This is the mechanical sweep instead.
//!
//! # What it cannot do
//!
//! "Public API reachable only from `#[cfg(test)]`" — the H.264 case — is not
//! attempted directly: a sound version needs a whole-program call graph
//! (what actually calls a given `pub fn`, through trait objects and
//! re-exports) that this dependency-free binary cannot build, and false
//! positives from trait dispatch would train people to silence a hard gate.
//!
//! [`check_unregistered_descriptors`] catches the specific shape the H.264
//! incident had instead: a fully-built, `pub` descriptor constant
//! (`DecoderDesc`/`EncoderDesc`/`DemuxerDesc`/`MuxerDesc`/`ParserDesc`/
//! `FilterDesc`/`ProtocolDesc`) that no `vaco-component.toml` fragment's
//! `ctor` names. It fired on exactly that shape this session —
//! `vaco-codec-opus::DECODER_OPUS`, real and complete for mono audio, kept
//! unregistered on purpose because the decoder is wrong for stereo content
//! (see [`ALLOW_ORPHAN_CRATE`]). What it still cannot catch: an
//! implementation exposed only as a bare function with **no descriptor
//! constant at all**, reachable only from a test that constructs the type
//! directly. That case still needs a person reading the crate.
//!
//! # The other four rules
//!
//! - **A** [`check_orphan_crates`] — a `crates/{codec,format,filter,io}`
//!   crate with no `vaco-component.toml` of its own and no other in-workspace
//!   crate depending on it. The `vaco-codec-opus` case, generalised.
//! - **B** [`check_nondefault_features`] — every component feature that opts
//!   out of `default` (patent gating, or a wasm-portability opt-out) must
//!   actually compile in isolation, or it can never be constructed in *any*
//!   build configuration, deliberate opt-out or not.
//! - **C** [`check_bsf_chaining`] — `vaco-registry`'s hand-assembled
//!   `bsf_descs()` must chain every `vaco-bsf-*` crate that registers a
//!   `bitstream_filter` component. The original `bsf_descs()` bug,
//!   generalised so a ninth `vaco-bsf-*` crate cannot repeat it silently.
//! - **D** [`check_muxer_only`] — a muxer with no demuxer of the same name is
//!   either legitimately write-only (a hash sink, a segmenter, a streaming
//!   wrapper) or it is the FLAC case: a real container with nothing on the
//!   read side. Every write-only name needs a row in [`ALLOW_MUXER_ONLY`]
//!   saying which.
//! - **F** [`check_filter_dispatch`] — a crate's `filter` and
//!   `filter_dispatch` components must appear together. The v360/#497 case:
//!   registered (so `-filters` lists it) with no dispatch path (so `-vf`
//!   cannot build it) is the same bug as the reverse, a `FilterRegistry`
//!   nothing points at.
//! - **G** [`check_decoder_reachable`]/[`check_encoder_reachable`] — a
//!   registered decoder/encoder whose `CodecId` no demuxer/muxer anywhere in
//!   the tree ever constructs. A second, independent way to ship the QOA
//!   incident's shape: `vaco-codec-simple-audio::QOA_DECODER` was registered
//!   and listed by `-decoders` — the registry was perfectly consistent — but
//!   no demuxer anywhere ever produced a `CodecId::Qoa` packet, because the
//!   decoder's own module doc says file framing is "a container concern"
//!   and, until this rule's fix landed, nothing provided one. Rule E already
//!   catches "the registry doesn't know this descriptor exists"; this rule
//!   catches "the registry knows about it, and it is still dead" — a
//!   consistent registry is not the same claim as a reachable one.
//! - **H** [`check_reference_names`] — a *third* independent way to ship a
//!   dead-but-consistent component: registered, listed, reachable by rule G,
//!   and still unusable because a real user would never type its name.
//!   `vaco-codec-jpeg` was registered as `jpeg` and reachable by every rule
//!   above — `ffmpeg -decoders`/`-encoders` (measured against the installed
//!   9.0.1, `xtask/data/reference-formats.txt`) has no decoder or encoder
//!   literally named `jpeg` at all, only `mjpeg`, so `-c:v mjpeg` — the name
//!   every real ffmpeg file or user actually uses — could not select it.
//!   `vaco-codec-subtitle-teletext` was `teletext` where the reference's own
//!   codec table (probed with `ffmpeg -h decoder=<name>`, which distinguishes
//!   a name FFmpeg recognises but cannot build from one it does not know at
//!   all) calls it `dvb_teletext`. Both fixed; [`ALLOW_NAME_MISMATCH`] is
//!   where a checked survivor goes, with which of those two measured
//!   outcomes justifies it.
//!
//! # Allowlists
//!
//! Same discipline as [`crate::dup_check`]'s `DISTINCT` and
//! [`crate::owner_gate`]'s `MEDIA`: every entry is a claim, in writing, about
//! why a specific gap is deliberate rather than a bug.

use std::process::Command;

use crate::{Map, Set, Task, crates, repo_root, rust_files};

// ------------------------------------------------------------- the fragments

/// The one `[[component]]` table fields this gate needs. A deliberately
/// smaller read of `vaco-component.toml` than `xtask/src/registry.rs`'s own
/// `Component` — this gate does not assemble the registry, it only asks
/// questions of what is already declared, so it does not need `long_name`,
/// `mime_types`, `media` or `encumbered`. `codec` is read (rule G only) for
/// a decoder/encoder row's `CodecId`; a demuxer/muxer row never sets it
/// (containers do not declare a fixed codec list in the fragment schema —
/// rule G finds what a demuxer/muxer actually constructs by reading its
/// source, not by a field here).
struct Row {
    /// The crate that declared it, dash-cased (`vaco-codec-opus`).
    krate: String,
    kind: String,
    /// A descriptor `name` may be a comma-separated alias family; every
    /// element is its own name for lookup purposes.
    names: Vec<String>,
    ctor: String,
    feature: Option<String>,
    default_on: bool,
    /// A decoder/encoder row's `codec = "..."` (the `CodecId::name()`
    /// string, e.g. `"qoa"`). `None` for every other kind.
    codec: Option<String>,
}

fn split_list(v: Option<&str>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every `[[component]]` row in the tree, from every crate's own fragment.
///
/// # Errors
/// A malformed fragment's message, naming the file and line — same reader
/// `cargo xtask gen-registry` uses, so a fragment that fails `gen-registry`
/// fails here with the same message rather than a second, different one.
fn all_rows() -> Result<Vec<Row>, String> {
    let mut out = Vec::new();
    for (_area, name, path) in crates() {
        let frag = path.join("vaco-component.toml");
        if !frag.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&frag).map_err(|e| format!("{}: {e}", frag.display()))?;
        let tables =
            crate::toml::tables(&text, &["component"]).map_err(|e| format!("{}: {e}", frag.display()))?;
        for t in tables {
            out.push(Row {
                krate: name.clone(),
                kind: t.get("kind").unwrap_or_default().to_owned(),
                names: split_list(t.get("name")),
                ctor: t.get("ctor").unwrap_or_default().to_owned(),
                feature: t.get("feature").map(str::to_owned),
                default_on: t.get("default") != Some("false"),
                codec: t.get("codec").map(str::to_owned),
            });
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ rule A

/// `crates/{codec,format,filter,io}` crates deliberately left with no
/// fragment and no in-workspace caller, and why each is not simply
/// `vaco-codec-opus` again.
///
/// Every reason here was read from the crate's own module doc, not invented
/// for this list — each of these already explained itself in writing before
/// this gate existed to ask.
const ALLOW_ORPHAN_CRATE: &[(&str, &str)] = &[
    (
        "vaco-codec-opus",
        "measured wrong, not merely unregistered: this crate's own \
         `DECODER_OPUS` doc claimed it 'ships in the default build' and \
         named a `vaco-component.toml` that never existed (this gate's own \
         motivating case). Registering it was tried this session and \
         reverted after measuring against `ffmpeg`/`libopus`: a mono 440 Hz \
         tone decodes correctly (RMS ratio 1.006 against ffmpeg's own \
         decode, best-aligned), but the same tone encoded stereo (libopus's \
         default coupled-stereo CELT mode, mapping family 0) decodes at \
         almost exactly **2x** the reference amplitude — consistent across \
         a 2000-sample window at the best time alignment, not a scatter. \
         Zero-crossing count matches (frequency is right), only amplitude is \
         wrong, which points at a specific spot: `StreamDecoder::decode_one_frame`'s \
         `Mode::CeltOnly` path with `channels == 2`, i.e. `celt::decode`'s \
         stereo reconstruction, not the mono CELT core (proven correct by \
         the mono measurement) and not `mix_to_output` (a plain copy for \
         mapping family 0). D19's ruling is explicit: registering a \
         component that produces wrong output is worse than leaving it \
         unreachable. Left unregistered until the stereo path is fixed and \
         re-measured.",
    ),
    (
        "vaco-cbs-jpeg",
        "coded-bitstream-syntax substrate (D-21b), exposes no component \
         descriptor at all — meant to be called by a future JPEG encoder that \
         needs bit-exact marker-segment rewriting. Exercised today only by its \
         own tests and `fuzz/fuzz_targets/cbs_jpeg.rs`. D19's scheduled `cbs` \
         unification names this crate directly.",
    ),
    (
        "vaco-cbs-vp9",
        "same shape as vaco-cbs-jpeg above, for VP9 (D-21a): its own module \
         doc says it depends on nothing from vaco-parse-vpx or vaco-codec-vp9 \
         deliberately, because both were under active ownership elsewhere when \
         it was written, and names no consumer yet.",
    ),
    (
        "vaco-format-apetag",
        "APEv1/APEv2 tag and ReplayGain helper. Its own module doc names the \
         demuxers meant to call it (ape/wv/mpc/tta/mp3) and states plainly \
         that none of them exist yet in this workspace — SH-08/SH-09's brief \
         explicitly did not require landing the callers first.",
    ),
    (
        "vaco-format-avlanguage",
        "language-code normalisation helper. Its own module doc names \
         vaco-demux-matroska/mp4/mxf/flv/asf/mpegts as the intended callers, \
         and unlike vaco-format-apetag those demuxers already exist — none of \
         them call in yet. A real, disclosed gap this pass did not close: \
         wiring it touches five demuxer crates this session did not open, and \
         verifying no existing ad-hoc language handling in any of them would \
         regress is more than a registry-reachability pass should take on \
         without owning those crates. Left for a dedicated follow-up.",
    ),
    (
        "vaco-codec-subtitle-cc",
        "CEA-608/708 decode. Its own module doc names two closing conditions \
         this crate is *not*: nothing upstream extracts `cc_data` from a \
         compressed stream yet (an H.264/HEVC/MPEG-2 parser change, not this \
         crate's), and wiring `vaco_codec_core::Decoder` is named explicit, \
         disclosed follow-up work, not attempted here to avoid landing a \
         `vaco-component.toml` fragment hastily in a shared tree (a bad \
         fragment breaks `gen-registry` for every agent, per \
         `planning/AGENT-CONSTRAINTS.md`).",
    ),
    (
        "vaco-codec-subtitle-teletext",
        "EBU/ETSI Teletext decode. Its own module doc section \
         '# No registry-to-decoder path — by design, not oversight' explains \
         that `vaco_frame::FrameData` had no `Subtitle` variant when this was \
         written, and a fragment naming a `kind = \"decoder\"` ctor here \
         would either lie about what it produces or fail the registry's own \
         descriptor-resolution check.",
    ),
    (
        "vaco-codec-subtitle-text",
        "Text subtitle markup decode (SubRip/ASS/WebVTT/mov_text/TTML). Its \
         own module doc section '# Not a `Decoder` implementation, \
         deliberately' says wiring is 'a small, mechanical follow-up' not \
         done here because `vaco_frame::FrameData::Subtitle` was uncommitted \
         work in another agent's tree at the time it was written.",
    ),
    (
        "vaco-protocol-rtp",
        "RTP/RTCP protocol-layer wrapper (issue #551). Its own module doc \
         says there is no `rtp:`/`rtcp:` registry entry yet; framing itself \
         lives one layer down in `vaco-rtp`.",
    ),
    (
        "vaco-protocol-srt",
        "SRT (issue #555). Its own module doc says the same as \
         vaco-protocol-rtmp's: 'a transport library, not yet a \
         `vaco_protocol_core::Protocol` — there is no `srt:`/`srts:` registry \
         entry here, no socket, and no cipher', pending a socket/timer seam \
         and an AES/CTR ownership decision named in `planning/INTERFACE-GAPS.md`.",
    ),
    (
        "vaco-protocol-rist",
        "RIST Simple/Main Profile (issues #558/#559). Same staged status as \
         the other new protocol crates below — a framing/session-state \
         library one PR away from a `vaco_protocol_core::Protocol`.",
    ),
    (
        "vaco-protocol-rtmp",
        "RTMP chunk stream, AMF0, NetConnection/NetStream (issues \
         #552-#554). Its own module doc says so directly: 'it is still a \
         transport-framing library, not yet a `vaco_protocol_core::Protocol` \
         — there is no `rtmp:`/`rtmps:` registry entry, because that needs \
         socket ownership this crate does not have.'",
    ),
    (
        "vaco-protocol-sctp",
        "SCTP framing and association state machine (issue #561). Its own \
         module doc names the same 'framing/state library, no `Protocol` \
         yet' stage as vaco-protocol-srt/rist/rtmp.",
    ),
    (
        "vaco-codec-av1",
        "intra-only AV1 decode (OBU through reconstructed pixels for a key \
         or intra-only frame). Its own module doc states inter prediction is \
         explicitly out of scope, left to later work. Registering `av1` today \
         would silently produce wrong output on any inter-predicted frame — \
         most real AV1 content — which this project's own ruling names as \
         worse than leaving it unreachable. See rule E below: this crate's \
         `AV1_DECODER` const is real and complete for the intra path; it \
         stays unregistered for correctness, not because it does not exist.",
    ),
    (
        "vaco-format-fixtures",
        "real, measured codec configuration-record bytes shared by \
         container test suites (planning/E2E-GAPS.md #35) -- every consumer \
         (vaco-cli, vaco-mux-matroska, vaco-demux-ogg, vaco-mux-ogg) takes it \
         as a dev-dependency only, by design: it exists to stop test \
         fixtures drifting from each other, not to ship in a real build, so \
         path_deps() (deliberately [dependencies]-only, see its own doc) \
         never sees the edges that make this crate not an orphan.",
    ),
];

fn check_orphan_crates() -> Result<Vec<String>, String> {
    const AREAS: &[&str] = &["codec", "format", "filter", "io"];

    let all = crates();
    let has_fragment: Set<String> = all
        .iter()
        .filter(|(_, _, p)| p.join("vaco-component.toml").exists())
        .map(|(_, n, _)| n.clone())
        .collect();

    let mut depended_on: Set<String> = Set::new();
    for (_, _, path) in &all {
        let manifest = std::fs::read_to_string(path.join("Cargo.toml")).unwrap_or_default();
        depended_on.extend(path_deps(&manifest));
    }

    let mut violations = Vec::new();
    for (area, name, _path) in &all {
        if !AREAS.contains(&area.as_str()) {
            continue;
        }
        if has_fragment.contains(name) || depended_on.contains(name) {
            continue;
        }
        if ALLOW_ORPHAN_CRATE.iter().any(|(n, _)| n == name) {
            continue;
        }
        violations.push(format!(
            "  crates/{area}/{name}: no `vaco-component.toml` of its own, and no \
             other crate in the workspace depends on it — this is the \
             `vaco-codec-opus` shape of bug. Either register it, wire a real \
             caller, or add it to ALLOW_ORPHAN_CRATE in \
             xtask/src/reachability_check.rs with a reason."
        ));
    }
    violations.sort();
    Ok(violations)
}

/// `vaco-*` path dependencies declared in one manifest's plain `[dependencies]`
/// table — deliberately not `[dev-dependencies]`/`[build-dependencies]`
/// (neither ships, so neither makes a crate reachable from the CLI), and
/// deliberately not the generated `[dependencies.<name>]` per-table style
/// `vaco-registry`'s own manifest uses (a crate reachable only that way
/// already has a fragment, by construction of `cargo xtask gen-registry` —
/// see [`all_rows`]'s callers).
fn path_deps(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let name: String = t
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if name.starts_with("vaco-") && t.contains("path") {
            out.push(name);
        }
    }
    out
}

// ------------------------------------------------------------------ rule B

/// Non-default component features, and why each opts out of `default`.
///
/// This does **not** exempt a feature from the compile check in
/// [`check_nondefault_features`] — it only requires a feature that is off by
/// default to say why, the same discipline `dup-check`'s `DISTINCT` and
/// `owner-gate`'s `MEDIA` apply. A feature failing to compile is a violation
/// regardless of whether it is listed here.
const ALLOW_NONDEFAULT_FEATURE: &[(&str, &str)] = &[
    (
        "patent-encumbered-aac-decode",
        "D4/D4.1: legally RED (Via LA AAC patent pool charges per decoder unit).",
    ),
    (
        "patent-encumbered-h264-decode",
        "D4/D4.1: legally RED (MPEG LA / Access Advance AVC patent pool).",
    ),
    (
        "patent-encumbered-hevc-decode",
        "D4/D4.1: legally RED (Access Advance/HEVC Advance, MPEG LA, Velos Media pools).",
    ),
    (
        "patent-encumbered-vc1-decode",
        "D4/D4.1: patent-encumbered (Microsoft/MPEG-LA-administered VC-1 pool).",
    ),
    (
        "protocol-http",
        "wasm portability: depends on `vaco-protocol-socket` (`socket2`), \
         `NATIVE_ONLY` per `cargo xtask wasm-check`. Measured directly: \
         registering this without `default = false` regressed `vaco-cli`'s \
         wasm32-unknown-unknown build.",
    ),
    (
        "protocol-httpproxy",
        "same measurement and reasoning as protocol-http above.",
    ),
    ("protocol-ftp", "same measurement and reasoning as protocol-http above."),
    ("protocol-icecast", "same measurement and reasoning as protocol-http above."),
    ("protocol-tls", "same measurement and reasoning as protocol-http above."),
    ("protocol-dtls", "same measurement and reasoning as protocol-http above."),
    ("protocol-socket", "same measurement and reasoning as protocol-http above."),
    ("protocol-gopher", "same measurement and reasoning as protocol-http above."),
    (
        "demux-rtp",
        "same wasm/native-only reasoning as vaco-protocol-socket's, per \
         vaco-demux-rtsp's own fragment comment.",
    ),
    ("demux-rtsp", "same wasm/native-only reasoning, same fragment comment as demux-rtp."),
    ("demux-sdp", "same wasm/native-only reasoning, same fragment comment as demux-rtp."),
    (
        "mux-whip",
        "same wasm/native-only reasoning as vaco-protocol-dtls's, per \
         vaco-mux-whip's own fragment comment.",
    ),
];

fn check_nondefault_features(rows: &[Row]) -> Result<Vec<String>, String> {
    let mut features: Set<String> = Set::new();
    for r in rows {
        if !r.default_on && let Some(f) = &r.feature {
            features.insert(f.clone());
        }
    }

    let mut violations = Vec::new();
    for f in &features {
        if !ALLOW_NONDEFAULT_FEATURE.iter().any(|(n, _)| n == f) {
            violations.push(format!(
                "  `{f}` is `default = false` with no recorded reason in \
                 ALLOW_NONDEFAULT_FEATURE (xtask/src/reachability_check.rs). \
                 D4 patent gating and the wasm-portability opt-out are the two \
                 reasons this tree uses today; if this is neither, it probably \
                 should be in `default`."
            ));
        }
    }

    // The check with actual teeth: a feature nobody's default build ever
    // compiles can silently bitrot into something that cannot be constructed
    // in *any* configuration — which is worse than being off by default, and
    // `--check`-only text auditing above cannot see it.
    for f in &features {
        let out = Command::new("cargo")
            .current_dir(repo_root())
            .args([
                "check",
                "-p",
                "vaco-registry",
                "--no-default-features",
                "--features",
                f,
                "--target-dir",
                "/tmp/vaco-reachability-check",
                "-q",
            ])
            .output()
            .map_err(|e| format!("cargo: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first_error = err
                .lines()
                .find(|l| l.starts_with("error"))
                .unwrap_or("(no error line found)");
            violations.push(format!(
                "  feature `{f}` does not compile in isolation \
                 (`cargo check -p vaco-registry --no-default-features --features \
                 {f}`) — every component behind it can never be constructed in \
                 this build configuration:\n    {first_error}"
            ));
        }
    }

    violations.sort();
    Ok(violations)
}

// ------------------------------------------------------------------ rule C

/// Find the source text of one top-level `fn` by its signature prefix,
/// balancing braces from the first `{`. Good enough for one known function in
/// one known file — not a Rust parser.
fn function_body(text: &str, sig: &str) -> Option<String> {
    let start = text.find(sig)?;
    let after = &text[start..];
    let brace = after.find('{')?;
    let mut depth = 0i32;
    for (i, c) in after.char_indices().skip(brace) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return after.get(..=i).map(str::to_owned);
                }
            }
            _ => {}
        }
    }
    None
}

fn check_bsf_chaining(rows: &[Row]) -> Result<Vec<String>, String> {
    let lib_path = repo_root().join("crates/registry/vaco-registry/src/lib.rs");
    let text = std::fs::read_to_string(&lib_path).map_err(|e| format!("{}: {e}", lib_path.display()))?;
    let Some(body) = function_body(&text, "fn bsf_descs()") else {
        return Err(format!(
            "{}: could not find `fn bsf_descs()` — it has moved or been \
             renamed, and xtask/src/reachability_check.rs's rule C needs \
             updating to match",
            lib_path.display()
        ));
    };

    let families: Set<&str> = rows
        .iter()
        .filter(|r| r.kind == "bitstream_filter")
        .map(|r| r.krate.as_str())
        .collect();

    let mut violations = Vec::new();
    for krate in families {
        let modpath = krate.replace('-', "_");
        if !body.contains(modpath.as_str()) {
            violations.push(format!(
                "  {krate} registers a `bitstream_filter` component, but \
                 `bsf_descs()` in crates/registry/vaco-registry/src/lib.rs \
                 never chains `{modpath}::filters()` — `Bsfs::open` can never \
                 reach it by name, whatever the registry's own component \
                 listing says. This is the original bsf_descs() bug \
                 generalised: add `.chain({modpath}::filters())`."
            ));
        }
    }
    violations.sort();
    Ok(violations)
}

// ------------------------------------------------------------------ rule D

/// Muxer names with no demuxer of the same name, and why each is genuinely
/// write-only rather than a FLAC-shaped gap.
///
/// Checked by **name**, not by extension: several demuxers here recognise
/// their format purely by content probe and declare no `extensions` at all
/// (`h263`, `dirac`, `swf`, `spdif`, `cavsvideo`), so comparing extension
/// lists directly flags those as false positives. Name equality is what
/// `-f <name>` and the registry's own `demuxer_by_name`/`muxer_by_name`
/// actually key on.
const ALLOW_MUXER_ONLY: &[(&str, &str)] = &[
    // Hash/checksum sinks: there is no bitstream to demux by construction —
    // the checksum *is* the output.
    ("crc", "hash-computing muxer; no bitstream to demux by definition."),
    ("framecrc", "hash-computing muxer; no bitstream to demux by definition."),
    ("framehash", "hash-computing muxer; no bitstream to demux by definition."),
    ("framemd5", "hash-computing muxer; no bitstream to demux by definition."),
    ("hash", "hash-computing muxer; no bitstream to demux by definition."),
    ("md5", "hash-computing muxer; no bitstream to demux by definition."),
    ("streamhash", "hash-computing muxer; no bitstream to demux by definition."),
    // Discard / wrapper muxers: not a format of their own.
    ("null", "discard-output muxer; nothing is written to read back."),
    (
        "fifo",
        "wraps another muxer for restart-on-failure; not a format of its own.",
    ),
    ("tee", "fans out to several other muxers; not a format of its own."),
    (
        "mkvtimestamp_v2",
        "Matroska external-timestamps export; a timing sidecar, not a media format.",
    ),
    // Segmenting/streaming wrappers, all output-only in ffmpeg too.
    (
        "segment",
        "segmenting wrapper muxer; writes numbered files for another format's \
         demuxer to read, not a format of its own.",
    ),
    ("ssegment", "same as `segment`, stream-copy variant."),
    ("stream_segment", "same as `segment`, generic streaming variant."),
    (
        "hds",
        "Adobe HTTP Dynamic Streaming muxer; output-only in ffmpeg too, no \
         demuxer of that name exists there either.",
    ),
    (
        "smoothstreaming",
        "MS Smooth Streaming muxer; output-only in ffmpeg too.",
    ),
    (
        "rtp_mpegts",
        "RTP-payloaded MPEG-TS streaming muxer; output-only, same as ffmpeg.",
    ),
    (
        "webm_chunk",
        "WebM DASH-chunk muxer; output-only, same as ffmpeg — a chunk reads \
         back as plain WebM through the `matroska` demuxer.",
    ),
    (
        "whip",
        "WHIP (WebRTC-HTTP Ingest) muxer; output-only signalling+media push, \
         no corresponding pull-side demux format.",
    ),
    // MPEG-PS / DVD-Video variants: read back through the generic `mpeg`
    // (MPEG-PS) demuxer, same as ffmpeg — none of these get their own
    // demuxer name there either.
    ("dvd", "MPEG-PS/DVD-Video variant; read back through the generic `mpeg` demuxer."),
    ("svcd", "MPEG-PS/SVCD variant; read back through the generic `mpeg` demuxer."),
    ("vcd", "MPEG-PS/VCD variant; read back through the generic `mpeg` demuxer."),
    ("vob", "MPEG-PS/VOB variant; read back through the generic `mpeg` demuxer."),
    // mp4-family and ogg-family aliases: read back through the shared
    // demuxer, whose `extensions` list already includes each alias's
    // extension (verified against both fragments, see the module doc).
    (
        "f4v",
        "mp4-family muxer alias; read back through the `mov,mp4,...` demuxer, \
         whose extensions already include `f4v`.",
    ),
    (
        "ipod",
        "mp4-family muxer alias (device profile); read back through the \
         `mov,mp4,...` demuxer, selected by content, not by muxer name.",
    ),
    (
        "psp",
        "mp4-family muxer alias (device profile); same as `ipod` above.",
    ),
    (
        "ismv",
        "mp4-family muxer alias (Smooth Streaming ISMV); read back through \
         the `mov,mp4,...` demuxer, whose extensions already include `ismv`.",
    ),
    (
        "oga",
        "Ogg-family muxer alias; read back through the `ogg` demuxer, whose \
         extensions already include `oga`.",
    ),
    (
        "ogv",
        "Ogg-family muxer alias; read back through the `ogg` demuxer, whose \
         extensions already include `ogv`.",
    ),
    (
        "opus",
        "Ogg Opus muxer alias; read back through the `ogg` demuxer, whose \
         extensions already include `opus` — unrelated to whether an Opus \
         *decoder* is registered (see rule A/E's `vaco-codec-opus` entries).",
    ),
    (
        "spx",
        "Ogg Speex muxer alias; read back through the `ogg` demuxer, whose \
         extensions already include `spx`.",
    ),
    (
        "asf_stream",
        "ASF live-streaming muxer variant; read back through the generic \
         `asf` demuxer, same as ffmpeg.",
    ),
];

fn check_muxer_only(rows: &[Row]) -> Vec<String> {
    let muxer_names: Set<&str> = rows
        .iter()
        .filter(|r| r.kind == "muxer")
        .flat_map(|r| r.names.iter().map(String::as_str))
        .collect();
    let demuxer_names: Set<&str> = rows
        .iter()
        .filter(|r| r.kind == "demuxer")
        .flat_map(|r| r.names.iter().map(String::as_str))
        .collect();

    let mut violations = Vec::new();
    for name in muxer_names {
        if demuxer_names.contains(name) {
            continue;
        }
        if ALLOW_MUXER_ONLY.iter().any(|(n, _)| *n == name) {
            continue;
        }
        violations.push(format!(
            "  muxer `{name}` has no demuxer of the same name, and is not in \
             ALLOW_MUXER_ONLY with a reason — files it writes may be \
             unreadable by this build even though the format is a real \
             container. This is the FLAC shape of bug: check whether a \
             demuxer should exist before assuming this one is write-only."
        ));
    }
    violations.sort();
    violations
}

// ------------------------------------------------------------------ rule E

/// The exact descriptor types a `ctor` can name (`xtask/src/registry.rs`'s
/// own `Kind::desc_ty` list, plus `bitstream_filter`'s untyped case is out of
/// scope here — rule C covers that one separately since it dispatches by
/// name, not by a `pub const`).
const DESC_TYPES: &[&str] = &[
    "DecoderDesc",
    "EncoderDesc",
    "DemuxerDesc",
    "MuxerDesc",
    "ParserDesc",
    "FilterDesc",
    "ProtocolDesc",
];

/// `crate::IDENT` pairs for a real, complete descriptor constant that no
/// fragment's `ctor` names, and why each is deliberate.
///
/// This is rule E's answer to the H.264 incident, generalised: every one of
/// these is a working (or honestly-non-working, see the DFPWM/ADPCM/AVIF
/// rows) descriptor sitting in its crate, unregistered. Each reason below was
/// read from the crate's own doc comment or fragment comment, not invented
/// for this list.
const ALLOW_UNREGISTERED_DESCRIPTOR: &[(&str, &str)] = &[
    (
        "vaco-codec-opus::DECODER_OPUS",
        "measured wrong for stereo content this session; see this crate's \
         ALLOW_ORPHAN_CRATE entry above for the full measurement (mono \
         correct, coupled-stereo CELT decode at ~2x amplitude).",
    ),
    (
        "vaco-cli::NULL_MUXER",
        "documented dead code: this module's own doc says it is superseded by \
         `vaco-mux-utility::MUXER_NULL`, which IS registered, and that \
         'nothing in exec.rs constructs them any more'. Kept rather than \
         deleted per this crate's standing instruction not to remove a module \
         another agent's work might still be reaching for.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G722_DECODER",
        "deliberately not ADPCM: this crate's own fragment comment says \
         `g722`/`g726`/`g726le` are 'a structurally different transform from \
         the real ITU-T algorithms, not a byte-inexact implementation of it — \
         registering it would hand a caller wrong output with no error'. Its \
         `SendReceive` impl always errors rather than decoding, by design.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G722_ENCODER",
        "same reason as ADPCM_G722_DECODER above.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G726_DECODER",
        "same reason as ADPCM_G722_DECODER above.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G726_ENCODER",
        "same reason as ADPCM_G722_DECODER above.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G726LE_DECODER",
        "same reason as ADPCM_G722_DECODER above.",
    ),
    (
        "vaco-codec-adpcm::ADPCM_G726LE_ENCODER",
        "same reason as ADPCM_G722_DECODER above.",
    ),
    (
        "vaco-codec-av1::AV1_DECODER",
        "intra-only; see its ALLOW_ORPHAN_CRATE entry above — registering it \
         would silently produce wrong output on inter-predicted frames.",
    ),
    (
        "vaco-codec-simple-audio::DFPWM_DECODER",
        "measured wrong: this crate's own module doc says plainly 'Not \
         implemented as a real codec' and that its predictor, transcribed \
         from the only public DFPWM1a write-up available, 'does not reproduce \
         ffmpeg 8.1's actual decode of a real .dfpwm' file. Its `SendReceive` \
         impl always errors rather than decoding.",
    ),
    (
        "vaco-codec-simple-audio::DFPWM_ENCODER",
        "same reason as DFPWM_DECODER above.",
    ),
    (
        "vaco-filter-core::DESC",
        "`src/mock.rs`'s own doc: 'No real filter exists yet, so this is how \
         the framework gets tested' — a worked example for the filter trait \
         contract, not a shipped filter.",
    ),
    (
        "vaco-filter-framesync::DESC",
        "same shape as vaco-filter-core::DESC above: a worked two-input \
         filter proving the framesync helpers, per `src/mock.rs`'s own doc.",
    ),
    (
        "vaco-format-core::DEMUXER",
        "`src/vacoraw.rs`'s own doc: 'not a format anybody should store media \
         in and it is not registered for general use... exists so that \
         vaco-demux-mp4 can be written against an interface that has already \
         been driven end to end'.",
    ),
    (
        "vaco-format-core::MUXER",
        "same worked-example reasoning as vaco-format-core::DEMUXER above.",
    ),
    (
        "vaco-mux-mp4::MUXER_AVIF",
        "`src/brand.rs`'s own doc: AVIF is a HEIF item structure \
         (`meta`/`iinf`/`iloc`/`iprp`/`ipco`/`pitm`), not a `moov`/`trak` \
         sample-table track, and this muxer's `open` always returns \
         `Unsupported` — kept only so the brand bytes have a name to register \
         from once a real HEIF item writer exists.",
    ),
    (
        "vaco-mux-mxf::MUXER_D10",
        "video-only today: its own doc says D-10's fixed 8-slot AES3 audio \
         bundle is not yet implemented on the write side.",
    ),
    (
        "vaco-mux-mxf::MUXER_OPATOM",
        "video-only today, same gap as MUXER_D10 above.",
    ),
];

fn check_unregistered_descriptors(rows: &[Row]) -> Vec<String> {
    let ctors: Set<&str> = rows.iter().map(|r| r.ctor.as_str()).collect();

    let mut violations = Vec::new();
    for (_area, name, path) in crates() {
        let modpath = name.replace('-', "_");
        for file in rust_files(&path.join("src")) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                let t = line.trim_start();
                for kw in ["pub const ", "pub static "] {
                    let Some(rest) = t.strip_prefix(kw) else {
                        continue;
                    };
                    let Some((ident, tyrest)) = rest.split_once(':') else {
                        continue;
                    };
                    let ident = ident.trim();
                    let ty = tyrest.trim();
                    if !DESC_TYPES.iter().any(|d| ty.starts_with(d)) {
                        continue;
                    }
                    // Skip `macro_rules!` templates (`pub static $ident: ...`):
                    // not a real identifier, and the macro's real invocations —
                    // each a distinct const under a name this line never
                    // states — are what actually needs checking, further down
                    // in the same file.
                    if !ident
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                        || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        continue;
                    }
                    // A ctor may sit in a submodule (`vaco_filter_blur::avgblur::DESC`),
                    // so this checks "declared under this crate, named exactly
                    // this identifier" rather than "declared at crate root" —
                    // matching by the ctor's own final `::` segment, not by
                    // reconstructing its full module path (which this
                    // line-based scan does not know).
                    let crate_prefix = format!("{modpath}::");
                    let registered = ctors
                        .iter()
                        .any(|c| c.starts_with(&crate_prefix) && c.rsplit("::").next() == Some(ident));
                    if registered {
                        continue;
                    }
                    let full = format!("{modpath}::{ident}");
                    let key = format!("{name}::{ident}");
                    if ALLOW_UNREGISTERED_DESCRIPTOR.iter().any(|(k, _)| *k == key) {
                        continue;
                    }
                    violations.push(format!(
                        "  {key} is a real {} but no `vaco-component.toml` \
                         fragment's `ctor` names `{full}` — it cannot be \
                         reached from the CLI. This is the H.264 shape of bug: \
                         register it (write a fragment) or add it to \
                         ALLOW_UNREGISTERED_DESCRIPTOR with a reason.",
                        ty.split(|c: char| !c.is_alphanumeric() && c != '_')
                            .next()
                            .unwrap_or(ty)
                    ));
                }
            }
        }
    }
    violations.sort();
    violations.dedup();
    violations
}

// ------------------------------------------------------------------ rule F

/// A `filter` component and a `filter_dispatch` component from the same
/// crate are two different, hand-written facts about the same filter family
/// (`-filters`/`-h filter=<name>` metadata versus the `FilterRegistry` impl
/// that actually builds one), so a fragment can state one without the
/// other. Either direction reproduces GitHub #497's shape: `v360` was
/// registered (listed, described) but had no dispatch path at all, so
/// `-filters` advertised it and `-vf v360=...` said "Unknown filter".
///
/// Once both facts come from the same fragment file this is a two-line
/// check rather than a person reading `vaco-cli/src/filterreg.rs` by hand.
fn check_filter_dispatch(rows: &[Row]) -> Vec<String> {
    let filter_crates: Set<&str> = rows
        .iter()
        .filter(|r| r.kind == "filter")
        .map(|r| r.krate.as_str())
        .collect();
    let dispatch_crates: Set<&str> = rows
        .iter()
        .filter(|r| r.kind == "filter_dispatch")
        .map(|r| r.krate.as_str())
        .collect();

    let mut violations = Vec::new();
    for krate in &filter_crates {
        if !dispatch_crates.contains(krate) {
            violations.push(format!(
                "  {krate} registers a `filter` component but no \
                 `filter_dispatch` one — its filters appear in `-filters` but \
                 `Filters::create` (`vaco-registry`) can never build them, the \
                 v360/#497 shape exactly. Add `[[component]] kind = \
                 \"filter_dispatch\"` naming {krate}'s `FilterRegistry` impl."
            ));
        }
    }
    for krate in &dispatch_crates {
        if !filter_crates.contains(krate) {
            violations.push(format!(
                "  {krate} registers a `filter_dispatch` component but no \
                 `filter` component — a `FilterRegistry` nothing in \
                 `-filters` ever points at. Remove the dead registration, or \
                 add the `filter` components it dispatches."
            ));
        }
    }
    violations.sort();
    violations
}

// ------------------------------------------------------------------ rule G

/// `CodecId::Variant` → `CodecId::name()`'s lowercase string (`"Qoa"` →
/// `"qoa"`), read directly from `vaco-codec-core`'s own `CODECS` table
/// rather than reimplemented as a PascalCase-to-snake_case guess — several
/// entries (`AacLatm` → `"aac_latm"`, `Eac3` → `"eac3"`) do not follow one
/// mechanical rule, so the table itself is the only reliable source.
fn codec_name_table() -> Result<Map<String, String>, String> {
    let path = repo_root().join("crates/signal/vaco-codec-core/src/lib.rs");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let start = text.find("const CODECS: &[CodecEntry] = &[").ok_or_else(|| {
        format!(
            "{}: could not find `const CODECS` — reachability rule G needs \
             updating to match wherever it moved",
            path.display()
        )
    })?;
    let body = &text[start..];
    let end = body.find("\n];").ok_or_else(|| {
        format!(
            "{}: `const CODECS` has no `\\n];` closing it within this file — \
             reachability rule G needs updating",
            path.display()
        )
    })?;
    let body = &body[..end];

    let mut out = Map::new();
    let mut rest = body;
    while let Some(i) = rest.find("CodecId::") {
        rest = &rest[i + "CodecId::".len()..];
        let variant_end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let variant = rest[..variant_end].to_owned();
        rest = &rest[variant_end..];
        let Some(q1) = rest.find('"') else { break };
        let after_q1 = &rest[q1 + 1..];
        let Some(q2) = after_q1.find('"') else { break };
        let name = after_q1[..q2].to_owned();
        rest = &after_q1[q2 + 1..];
        out.insert(variant, name);
    }
    if out.is_empty() {
        return Err(format!(
            "{}: parsed zero `CodecId::Variant, \"name\"` pairs out of `const \
             CODECS` — the table's shape changed and rule G's line scanner \
             needs updating to match",
            path.display()
        ));
    }
    Ok(out)
}

/// `krate` plus every `vaco-*` crate it path-depends on, transitively.
///
/// Needed because a container's codec detection is not always inline in the
/// crate that registers its `demuxer`/`muxer` component: `vaco-demux-mp4`
/// registers the `mov,mp4,...` demuxer but the FourCC → `CodecId` mapping
/// lives in `vaco-format-isom`, a plain path dependency. One hop is not
/// enough in general (a shared table crate could itself delegate further),
/// so this walks the full closure rather than assuming a fixed depth.
fn transitive_crate_closure(krate: &str, all: &[(String, String, std::path::PathBuf)]) -> Set<String> {
    let manifest_of: Map<&str, &std::path::Path> =
        all.iter().map(|(_, n, p)| (n.as_str(), p.as_path())).collect();
    let mut seen: Set<String> = Set::new();
    let mut stack = vec![krate.to_owned()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(&p) = manifest_of.get(n.as_str()) {
            let manifest = std::fs::read_to_string(p.join("Cargo.toml")).unwrap_or_default();
            for dep in path_deps(&manifest) {
                if !seen.contains(&dep) {
                    stack.push(dep);
                }
            }
        }
    }
    seen
}

/// Every `CodecId::Variant` token found in the given crates' own source,
/// mapped through `variant_to_name` to the lowercase names a decoder/encoder
/// row's `codec` field uses — i.e. every codec some container in this set of
/// crates actually constructs a packet for or accepts one from.
///
/// A `//`-prefixed line (including `///`/`//!` doc comments) is skipped
/// before scanning it: this is a textual scan, not a parser, and the one
/// false hit found writing this rule (`vaco-format-core`'s own module doc,
/// `//! let idx = mux.add_stream(&CodecParameters::video().with_codec(
/// CodecId::H264))?;`) was exactly a doc example, not code that runs. This
/// does not attempt to also skip `#[cfg(test)]` bodies — a codec mentioned
/// only inside a test helper is a false pass this rule can still miss, but
/// every instance found writing it names a codec with real production
/// support elsewhere too, so it costs nothing measured today; a person
/// reading a specific report is still the backstop
/// [`ALLOW_UNDEMUXABLE_DECODER`]/[`ALLOW_UNMUXABLE_ENCODER`] exist for.
fn codecs_referenced_in(crate_names: &Set<String>, variant_to_name: &Map<String, String>) -> Set<String> {
    let all = crates();
    let paths: Vec<&std::path::Path> = all
        .iter()
        .filter(|(_, n, _)| crate_names.contains(n))
        .map(|(_, _, p)| p.as_path())
        .collect();

    let mut out = Set::new();
    for base in paths {
        for file in rust_files(&base.join("src")) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let mut rest = line;
                while let Some(i) = rest.find("CodecId::") {
                    rest = &rest[i + "CodecId::".len()..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len());
                    let variant = &rest[..end];
                    if let Some(name) = variant_to_name.get(variant) {
                        out.insert(name.clone());
                    }
                    rest = &rest[end..];
                }
            }
        }
    }
    out
}

/// Registered decoders whose `CodecId` no demuxer anywhere in the tree ever
/// constructs, and why each is legitimately fine as-is.
///
/// A codec with genuinely no standalone container of its own (only ever
/// carried inside another, transport-layer, or system-level format this
/// build does not implement) belongs here with that stated plainly — that
/// is not the QOA shape of bug, it is simply out of scope. "No container
/// carries it, anywhere" is the shape this rule exists to catch.
const ALLOW_UNDEMUXABLE_DECODER: &[(&str, &str)] = &[];

/// Registered encoders whose `CodecId` no muxer anywhere in the tree ever
/// accepts, and why each is legitimately fine as-is. See
/// [`ALLOW_UNDEMUXABLE_DECODER`]'s doc for the same reasoning, mirrored for
/// the write side.
const ALLOW_UNMUXABLE_ENCODER: &[(&str, &str)] = &[];

fn check_codec_reachable(
    rows: &[Row],
    leaf_kind: &str,
    container_kind: &str,
    variant_to_name: &Map<String, String>,
    allow: &[(&str, &str)],
    allow_name: &str,
) -> Vec<String> {
    let all = crates();
    let container_crates: Set<String> = rows
        .iter()
        .filter(|r| r.kind == container_kind)
        .map(|r| r.krate.clone())
        .collect();

    let mut universe: Set<String> = Set::new();
    for krate in &container_crates {
        universe.extend(transitive_crate_closure(krate, &all));
    }
    let producible = codecs_referenced_in(&universe, variant_to_name);

    let action = if leaf_kind == "decoder" { "decode" } else { "encode" };

    let mut violations = Vec::new();
    for row in rows.iter().filter(|r| r.kind == leaf_kind) {
        let Some(codec) = row.codec.as_deref() else {
            continue;
        };
        if producible.contains(codec) || allow.iter().any(|(n, _)| *n == codec) {
            continue;
        }
        let names = row.names.join(",");
        violations.push(format!(
            "  {}::{names} ({leaf_kind}, codec `{codec}`) is registered, but no \
             {container_kind} anywhere in the tree ever references `CodecId` \
             for `{codec}` — nothing can produce the packets this {leaf_kind} \
             would {action}. This is the QOA shape of bug: a registry that is \
             perfectly consistent and still dead. Either add a {container_kind} \
             that carries `{codec}`, or add `{codec}` to {allow_name} with a \
             reason (e.g. it only ever appears inside another format this \
             build does not carry standalone).",
            row.krate,
        ));
    }
    violations.sort();
    violations
}

fn check_decoder_reachable(rows: &[Row], variant_to_name: &Map<String, String>) -> Vec<String> {
    check_codec_reachable(
        rows,
        "decoder",
        "demuxer",
        variant_to_name,
        ALLOW_UNDEMUXABLE_DECODER,
        "ALLOW_UNDEMUXABLE_DECODER",
    )
}

fn check_encoder_reachable(rows: &[Row], variant_to_name: &Map<String, String>) -> Vec<String> {
    check_codec_reachable(
        rows,
        "encoder",
        "muxer",
        variant_to_name,
        ALLOW_UNMUXABLE_ENCODER,
        "ALLOW_UNMUXABLE_ENCODER",
    )
}

// ------------------------------------------------------------------ rule H

/// One `[section]`'s bare names from `xtask/data/reference-formats.txt`,
/// comma-joined alias families split the same way [`Row::names`] are — the
/// reference's own multi-name rows (`"matroska,webm"`, `"mov,mp4,m4a,3gp,
/// 3g2,mj2"`) are one line each in that file, matching its own `-demuxers`/
/// `-muxers` output verbatim.
fn reference_section(text: &str, section: &str) -> Set<String> {
    let marker = format!("[{section}]\n");
    let Some(start) = text.find(&marker) else {
        return Set::new();
    };
    let body = &text[start + marker.len()..];
    let end = body.find("\n[").unwrap_or(body.len());
    let mut out = Set::new();
    for line in body[..end].lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for tok in line.split(',') {
            let tok = tok.trim();
            if !tok.is_empty() {
                out.insert(tok.to_owned());
            }
        }
    }
    out
}

/// A registered name absent from the reference's own measured name list
/// (`xtask/data/reference-formats.txt`), and why keeping it is deliberate
/// rather than an oversight — the `jpeg`/`mjpeg` and `teletext`/
/// `dvb_teletext` shape (both fixed, not listed here) generalised into a
/// gate that cannot un-catch itself.
///
/// Two genuinely different reasons show up below, and the distinction
/// matters for whoever next re-measures this list against a different
/// reference build:
///
/// - **Correctly named, absent from this specific binary.** `ffmpeg -h
///   decoder=X`/`-h encoder=X` answers "known to FFmpeg, but no
///   decoders/encoders for it are available" for a name FFmpeg's own codec
///   table recognises but this build cannot construct (an optional library
///   — libjxl, libwebp's encoder, libzvbi — not compiled in), distinct from
///   "is not recognized by FFmpeg" for a name that is simply wrong (the
///   `teletext`/`dvb_teletext` case this rule exists to catch). Filters,
///   bitstream filters and protocols have no equivalent distinguishing
///   message, so a missing name there is corroborated instead by public,
///   freely-reusable FFmpeg documentation (D7 Tier A: CLI names and their
///   documented semantics) naming the same string, or left an open question
///   when neither source resolves it.
/// - **Genuinely different implementation, on purpose.** `vaco-filter-motion`
///   built its own `stabdetect`/`stabtransform` because the reference's
///   `vidstabdetect`/`vidstabtransform` need `libvidstab` (GPL, D3), and no
///   reference binary anywhere to probe carries it — not merely absent from
///   this build. `vaco-codec-vp8`/`vp9`'s encoders are native, not the
///   reference's `libvpx`-wrapped ones; there is no bare `vp8`/`vp9` encoder
///   name in the reference to collide with, so naming them after the codec
///   is not a fabricated name so much as the natural one.
const ALLOW_NAME_MISMATCH: &[(&str, &str)] = &[
    (
        "vp8",
        "encoder: the reference has no native VP8 encoder at all, only \
         `libvpx` (measured: `ffmpeg -c:v vp8 -f null -` succeeds, resolving \
         through the codec name to `libvpx` — the same mechanism `-h \
         encoder=vp8` demonstrates). vaco-codec-vp8's encoder is a genuinely \
         different, native (non-libvpx) implementation; there is no bare \
         `vp8` encoder name in the reference for this to be confused with.",
    ),
    (
        "vp9",
        "encoder: same as `vp8` above — the reference's only VP9 encoder is \
         `libvpx-vp9`; vaco-codec-vp9's is native.",
    ),
    (
        "webp",
        "encoder: `ffmpeg -h encoder=webp` reports 'known to FFmpeg, but no \
         encoders for it are available' — the right name, absent from this \
         build (needs a library this environment's ffmpeg was not compiled \
         with), not a wrong one.",
    ),
    (
        "qoa",
        "encoder: `ffmpeg -h encoder=qoa` reports 'known to FFmpeg, but no \
         encoders for it are available' — right name, no encoder built into \
         this reference binary.",
    ),
    (
        "v210x",
        "encoder: `ffmpeg -h encoder=v210x` reports 'known to FFmpeg, but no \
         encoders for it are available' — right name, no encoder built into \
         this reference binary.",
    ),
    (
        "jpegxl",
        "decoder: `ffmpeg -h decoder=jpegxl` reports 'known to FFmpeg, but \
         no decoders for it are available' — right name, this build lacks \
         libjxl.",
    ),
    (
        "dvb_teletext",
        "decoder: `ffmpeg -h decoder=dvb_teletext` reports 'known to \
         FFmpeg, but no decoders for it are available' — right name (this \
         rule's own motivating fix, renamed from the wrong `teletext`), no \
         decoder built into this reference binary (needs libzvbi).",
    ),
    (
        "ttml",
        "decoder: `ffmpeg -h decoder=ttml` reports 'known to FFmpeg, but no \
         decoders for it are available' — right name, no decoder built into \
         this reference binary. demuxer: `vaco-subtitle-text`'s own fragment \
         already documents this one as spec-only with no reference \
         counterpart at all (not merely absent from this build) — TTML is \
         muxed by the reference but never demuxed from a standalone file.",
    ),
    (
        "dash",
        "demuxer: the reference's own muxer list has `dash`, and DASH is a \
         well-documented, unambiguous format name (MPEG-DASH); most likely \
         gated on `libxml2`, not present in this build's configure flags, \
         the same shape as the codecs above but without an `-h demuxer=` \
         message to confirm it directly — recorded as the same class rather \
         than guessed at further.",
    ),
    (
        "imf",
        "demuxer: IMF (Interoperable Master Format) is a well-documented, \
         unambiguous professional-media format name with the same likely \
         `libxml2` gating as `dash` above, and the same lack of a \
         confirming `-h demuxer=` message.",
    ),
    (
        "ass",
        "filter: a well-documented real FFmpeg filter name (needs libass); \
         `-h filter=` gives no 'known but disabled' signal the way codecs \
         do, so this rests on public documentation (D7 Tier A) rather than \
         a direct measurement against this build.",
    ),
    (
        "subtitles",
        "filter: same as `ass` above — needs libass.",
    ),
    (
        "drawtext",
        "filter: a well-documented real FFmpeg filter name needing \
         libfreetype; same lack of an `-h filter=` disabled-vs-unknown \
         signal as `ass` above.",
    ),
    (
        "stabdetect",
        "filter: `vaco-filter-motion`'s own module doc (see `stabdetect.rs`) \
         already records the reason — `vidstabdetect` needs GPL `libvidstab`, \
         which this project will not link (D3), and no reference binary \
         anywhere to probe carries it either, so this is an independent \
         equivalent under its own name, not claiming `.trf` file \
         compatibility.",
    ),
    (
        "stabtransform",
        "filter: paired with `stabdetect` above; same reason.",
    ),
];

fn check_reference_names(rows: &[Row]) -> Result<Vec<String>, String> {
    let path = repo_root().join("xtask/data/reference-formats.txt");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;

    const KIND_TO_SECTION: &[(&str, &str)] = &[
        ("decoder", "decoders"),
        ("encoder", "encoders"),
        ("demuxer", "demuxers"),
        ("muxer", "muxers"),
        ("filter", "filters"),
        ("bitstream_filter", "bitstream_filters"),
        ("protocol", "protocols"),
    ];

    let mut violations = Vec::new();
    for &(kind, section) in KIND_TO_SECTION {
        let ref_names = reference_section(&text, section);
        for row in rows.iter().filter(|r| r.kind == kind) {
            for name in &row.names {
                if ref_names.contains(name) || ALLOW_NAME_MISMATCH.iter().any(|(n, _)| n == name) {
                    continue;
                }
                violations.push(format!(
                    "  {}::{name} ({kind}) — xtask/data/reference-formats.txt's \
                     [{section}] section has no `{name}`. Either this is the wrong \
                     name (for a decoder/encoder, measure `ffmpeg -h {kind}={name}`: \
                     \"is not recognized\" means genuinely wrong and should be \
                     fixed, matching every user's real ffmpeg vocabulary; \"known to \
                     FFmpeg, but no {kind}s available\" means the name is right and \
                     this build simply lacks the implementation) or the divergence \
                     is deliberate and belongs in ALLOW_NAME_MISMATCH with a \
                     measured reason.",
                    row.krate,
                ));
            }
        }
    }
    violations.sort();
    Ok(violations)
}

// ------------------------------------------------------------------- driver

pub fn run(_check: bool) -> Task {
    let rows = all_rows()?;
    let variant_to_name = codec_name_table()?;

    let sections: [(&str, Vec<String>); 9] = [
        (
            "A. crate with no fragment and no in-workspace caller",
            check_orphan_crates()?,
        ),
        (
            "B. non-default feature does not compile in isolation",
            check_nondefault_features(&rows)?,
        ),
        (
            "C. bitstream-filter family missing from bsf_descs()",
            check_bsf_chaining(&rows)?,
        ),
        ("D. muxer with no demuxer of the same name", check_muxer_only(&rows)),
        (
            "E. descriptor built but never registered",
            check_unregistered_descriptors(&rows),
        ),
        (
            "F. filter and filter_dispatch components disagree",
            check_filter_dispatch(&rows),
        ),
        (
            "G1. decoder's codec produced by no demuxer",
            check_decoder_reachable(&rows, &variant_to_name),
        ),
        (
            "G2. encoder's codec accepted by no muxer",
            check_encoder_reachable(&rows, &variant_to_name),
        ),
        (
            "H. registered name absent from the reference's own measured names",
            check_reference_names(&rows)?,
        ),
    ];

    let total: usize = sections.iter().map(|(_, v)| v.len()).sum();
    if total > 0 {
        let mut report = String::new();
        for (title, v) in &sections {
            if v.is_empty() {
                continue;
            }
            report.push_str(&format!("\n{title} ({}):\n{}\n", v.len(), v.join("\n")));
        }
        return Err(format!(
            "{total} reachability violation(s) across {} rule(s):\n{report}",
            sections.iter().filter(|(_, v)| !v.is_empty()).count()
        ));
    }

    let allowlisted = ALLOW_ORPHAN_CRATE.len()
        + ALLOW_NONDEFAULT_FEATURE.len()
        + ALLOW_MUXER_ONLY.len()
        + ALLOW_UNREGISTERED_DESCRIPTOR.len()
        + ALLOW_UNDEMUXABLE_DECODER.len()
        + ALLOW_UNMUXABLE_ENCODER.len()
        + ALLOW_NAME_MISMATCH.len();
    println!(
        "reachability-check: clean — {} components across {} fragments checked \
         by {} rules, {allowlisted} deliberate gap(s) on record",
        rows.len(),
        crates()
            .iter()
            .filter(|(_, _, p)| p.join("vaco-component.toml").exists())
            .count(),
        sections.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_orphan_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_ORPHAN_CRATE {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn every_nondefault_feature_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_NONDEFAULT_FEATURE {
            assert!(why.len() > 15, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn every_muxer_only_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_MUXER_ONLY {
            assert!(why.len() > 15, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn every_unregistered_descriptor_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_UNREGISTERED_DESCRIPTOR {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn every_undemuxable_decoder_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_UNDEMUXABLE_DECODER {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn every_unmuxable_encoder_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_UNMUXABLE_ENCODER {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    /// The regression this rule exists to catch: `CodecId::Jpeg`'s real name
    /// is `"mjpeg"`, not the mechanical PascalCase-to-snake_case guess
    /// `"jpeg"` — this is exactly why [`codec_name_table`] reads the table
    /// textually instead of reimplementing a naming rule.
    #[test]
    fn codec_name_table_has_known_non_mechanical_entries() {
        let table = codec_name_table().expect("vaco-codec-core's CODECS table parses");
        assert_eq!(table.get("H264").map(String::as_str), Some("h264"));
        assert_eq!(table.get("Jpeg").map(String::as_str), Some("mjpeg"));
        assert_eq!(table.get("AacLatm").map(String::as_str), Some("aac_latm"));
        assert!(table.len() > 50, "expected dozens of codecs, got {}", table.len());
    }

    #[test]
    fn every_name_mismatch_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_NAME_MISMATCH {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn reference_section_splits_comma_joined_alias_families() {
        let text = "[demuxers]\nmatroska,webm\nmov,mp4,m4a\n\n[muxers]\nmov\n";
        let demux = reference_section(text, "demuxers");
        assert!(demux.contains("matroska"));
        assert!(demux.contains("webm"));
        assert!(demux.contains("mov"));
        assert!(demux.contains("m4a"));
        assert!(!demux.contains("mov,mp4,m4a"), "must split, not keep the joined line");
        let mux = reference_section(text, "muxers");
        assert_eq!(mux, std::iter::once("mov".to_owned()).collect());
    }

    #[test]
    fn reference_section_ignores_comments_and_stops_at_the_next_section() {
        let text = "[decoders]\n# a comment\nh264\n\n[encoders]\naac\n";
        let dec = reference_section(text, "decoders");
        assert_eq!(dec, std::iter::once("h264".to_owned()).collect());
    }

    #[test]
    fn path_deps_reads_inline_dependency_tables() {
        let manifest = "[dependencies]\nvaco-core = { path = \"../../core/vaco-core\" }\n\
                        serde = \"1\"\n\n[dev-dependencies]\nvaco-test-support = { path = \"../x\" }\n";
        assert_eq!(path_deps(manifest), vec!["vaco-core".to_owned()]);
    }

    #[test]
    fn function_body_balances_nested_braces() {
        let text = "fn f() {\n    if true {\n        g();\n    }\n}\nfn h() {}\n";
        let body = function_body(text, "fn f()").expect("found");
        assert!(body.contains("g();"));
        assert!(!body.contains("fn h"));
    }
}
