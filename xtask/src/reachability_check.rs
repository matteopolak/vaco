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
//!
//! # Allowlists
//!
//! Same discipline as [`crate::dup_check`]'s `DISTINCT` and
//! [`crate::owner_gate`]'s `MEDIA`: every entry is a claim, in writing, about
//! why a specific gap is deliberate rather than a bug.

use std::process::Command;

use crate::{Set, Task, crates, repo_root, rust_files};

// ------------------------------------------------------------- the fragments

/// The one `[[component]]` table fields this gate needs. A deliberately
/// smaller read of `vaco-component.toml` than `xtask/src/registry.rs`'s own
/// `Component` — this gate does not assemble the registry, it only asks
/// questions of what is already declared, so it does not need `long_name`,
/// `mime_types`, `media`, `codec` or `encumbered`.
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

// ------------------------------------------------------------------- driver

pub fn run(_check: bool) -> Task {
    let rows = all_rows()?;

    let sections: [(&str, Vec<String>); 5] = [
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
        + ALLOW_UNREGISTERED_DESCRIPTOR.len();
    println!(
        "reachability-check: clean — {} components across {} fragments checked \
         by 5 rules, {allowlisted} deliberate gap(s) on record",
        rows.len(),
        crates()
            .iter()
            .filter(|(_, _, p)| p.join("vaco-component.toml").exists())
            .count()
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
