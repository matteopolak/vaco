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
//! - **I** [`check_unconsumed_options`] — a fourth independent way to ship a
//!   dead-but-consistent component: registered, listed, reachable by rule G,
//!   correctly named by rule H, and still lying to the user about one of its
//!   own knobs. The CLI's own option audit (`vaco-cli`/`vaco-cli-core`,
//!   another agent's lane) found 108 of 237 top-level flags parsed and then
//!   never read; this rule is the same defect one level down, on every
//!   `#[opt(...)]` field a component crate declares. It caught
//!   `vaco-filter-deinterlace::kerndeint`'s `map`, `vaco-filter-artistic::
//!   vignette`'s `dither` (whose declared default did not even match the
//!   code's own unconditional behaviour), `vaco-format-core::
//!   FormatOptions`'s ten generic fields and `vaco-demux-rtsp::
//!   RtspOptions`'s `user_agent`, among others — each fixed by either
//!   implementing the field or making a non-default value refuse by name,
//!   on the same "silently ignoring it is the worst outcome" rule the CLI
//!   audit used. Scoped to one *crate* at a time — not one file, and not the
//!   whole workspace — deliberately: see [`check_unconsumed_options`]'s own
//!   doc for the two shapes of false result each of the other two scopes
//!   produced in this tree before crate scope replaced them, both found by
//!   reading rather than by the scan.
//! - **J** [`check_decoder_exists_for_produced_codecs`] — rule G asks, for
//!   every registered decoder, whether some demuxer can produce its
//!   `CodecId`; this asks the question the other way round: for every
//!   `CodecId` some demuxer in the tree *can* produce, does a registered
//!   decoder handle it? Neither implies the other. It caught
//!   `vaco-demux-matroska` resolving an `A_PCM/*` track to the generic
//!   `CodecId::Pcm` — a real, spellable variant with no registered decoder
//!   anywhere, only its concrete `PcmS16le`/`PcmF64le`/etc. siblings have
//!   one — consistent with every rule above and unusable on a real file.
//!   Fixed by making the demuxer resolve to a concrete variant instead;
//!   [`ALLOW_UNDECODABLE_PRODUCED`] is where a codec goes when nothing in
//!   this tree decodes it yet, not where a fixable demuxer-resolution gap
//!   goes to hide.
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
        let text =
            std::fs::read_to_string(&frag).map_err(|e| format!("{}: {e}", frag.display()))?;
        let tables = crate::toml::tables(&text, &["component"])
            .map_err(|e| format!("{}: {e}", frag.display()))?;
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
    (
        "protocol-ftp",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "protocol-icecast",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "protocol-tls",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "protocol-dtls",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "protocol-socket",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "protocol-gopher",
        "same measurement and reasoning as protocol-http above.",
    ),
    (
        "demux-rtp",
        "same wasm/native-only reasoning as vaco-protocol-socket's, per \
         vaco-demux-rtsp's own fragment comment.",
    ),
    (
        "demux-rtsp",
        "same wasm/native-only reasoning, same fragment comment as demux-rtp.",
    ),
    (
        "demux-sdp",
        "same wasm/native-only reasoning, same fragment comment as demux-rtp.",
    ),
    (
        "mux-whip",
        "same wasm/native-only reasoning as vaco-protocol-dtls's, per \
         vaco-mux-whip's own fragment comment.",
    ),
];

fn check_nondefault_features(rows: &[Row]) -> Result<Vec<String>, String> {
    let mut features: Set<String> = Set::new();
    for r in rows {
        if !r.default_on
            && let Some(f) = &r.feature
        {
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
    let text =
        std::fs::read_to_string(&lib_path).map_err(|e| format!("{}: {e}", lib_path.display()))?;
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
    (
        "crc",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "framecrc",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "framehash",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "framemd5",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "hash",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "md5",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    (
        "streamhash",
        "hash-computing muxer; no bitstream to demux by definition.",
    ),
    // Discard / wrapper muxers: not a format of their own.
    (
        "null",
        "discard-output muxer; nothing is written to read back.",
    ),
    (
        "fifo",
        "wraps another muxer for restart-on-failure; not a format of its own.",
    ),
    (
        "tee",
        "fans out to several other muxers; not a format of its own.",
    ),
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
    (
        "stream_segment",
        "same as `segment`, generic streaming variant.",
    ),
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
    (
        "dvd",
        "MPEG-PS/DVD-Video variant; read back through the generic `mpeg` demuxer.",
    ),
    (
        "svcd",
        "MPEG-PS/SVCD variant; read back through the generic `mpeg` demuxer.",
    ),
    (
        "vcd",
        "MPEG-PS/VCD variant; read back through the generic `mpeg` demuxer.",
    ),
    (
        "vob",
        "MPEG-PS/VOB variant; read back through the generic `mpeg` demuxer.",
    ),
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
                    let registered = ctors.iter().any(|c| {
                        c.starts_with(&crate_prefix) && c.rsplit("::").next() == Some(ident)
                    });
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
    let start = text
        .find("const CODECS: &[CodecEntry] = &[")
        .ok_or_else(|| {
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
fn transitive_crate_closure(
    krate: &str,
    all: &[(String, String, std::path::PathBuf)],
) -> Set<String> {
    let manifest_of: Map<&str, &std::path::Path> = all
        .iter()
        .map(|(_, n, p)| (n.as_str(), p.as_path()))
        .collect();
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
/// CodecId::H264))?;`) was exactly a doc example, not code that runs.
///
/// [`mask_test_code`] also runs before scanning, added after rule I's own
/// audit found the same gap costing real findings elsewhere: a codec
/// mentioned only inside a `#[cfg(test)]` helper is a false pass a scan
/// without this would count as "a demuxer produces it" when nothing outside
/// a test does. Re-running with masking in place found no new violation in
/// this tree today — the original claim ("every instance found writing it
/// names a codec with real production support elsewhere too") still holds —
/// but the fix stays rather than reverting it, since a scan that only
/// happens to be right today is not the same claim as one that is right by
/// construction. A person reading a specific report is still the backstop
/// [`ALLOW_UNDEMUXABLE_DECODER`]/[`ALLOW_UNMUXABLE_ENCODER`] exist for.
fn codecs_referenced_in(
    crate_names: &Set<String>,
    variant_to_name: &Map<String, String>,
) -> Set<String> {
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
            let text = mask_test_code(&text);
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

    let action = if leaf_kind == "decoder" {
        "decode"
    } else {
        "encode"
    };

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

/// Registered as producible but genuinely not decodable yet -- a real gap,
/// declared so it is discovered here rather than by a user with a real
/// file. Every entry needs a reason a later reader can check, the same
/// discipline `dup-check`'s `DISTINCT` and `owner-gate`'s `MEDIA` apply.
///
/// **Not** the place for a codec a demuxer's own resolution could still
/// pick a concrete, decodable variant for and currently does not (the
/// `CodecId::Pcm` shape before this rule's own fix) -- that is a bug in
/// the demuxer, and an allowlist entry here would turn this rule from a
/// gate into a rubber stamp over it.
const ALLOW_UNDECODABLE_PRODUCED: &[(&str, &str)] = &[
    (
        "pcm",
        "vaco-demux-matroska::codec::resolve_pcm's own deliberate fallback \
         for a BitDepth this project has not measured a real ffmpeg \
         encoder produce (only 8/16/24/32-bit int and 32/64-bit float \
         resolve to a concrete, decodable CodecId::Pcm*); refusing to \
         guess a wire format for an unmeasured depth is the point, not a \
         gap to close by resolving further.",
    ),
    (
        "opus",
        "vaco-codec-opus is a real, complete decoder deliberately left \
         unregistered -- see rule A's own ALLOW_ORPHAN_CRATE entry for the \
         measurement: mono decodes correctly (RMS ratio 1.006 against \
         ffmpeg), stereo decodes at ~2x the reference amplitude \
         (CeltOnly's stereo reconstruction). D19: registering a component \
         that produces wrong output is worse than leaving it unreachable.",
    ),
    (
        "av1",
        "vaco-codec-av1's own module doc states the scope directly: intra \
         frames only. Inter prediction, deblocking/CDEF/superres/loop \
         restoration application, film grain, frame threading and DPB are \
         all named 'out of scope, left for later work' -- registering it \
         as a general decoder would silently produce pre-in-loop-filter, \
         inter-frame-rejecting output for virtually every real AV1 file.",
    ),
    (
        "anull",
        "vaco-codec-null's own fragment comment: 'vnull/anull are \
         encode-only per the roadmap's 0 dec / 2 enc accounting (plan 20 \
         §1.9, C-47 merged into issue #281) -- there is nothing to decode'.",
    ),
    (
        "vnull",
        "Same fragment, same reason as `anull` above -- the video half of \
         the same deliberate encode-only pair.",
    ),
    (
        "dfpwm",
        "vaco-codec-simple-audio's own `dfpwm` module doc records a real \
         measurement: the only public DFPWM1a write-up available does not \
         reproduce ffmpeg 8.1's actual decode of a real .dfpwm stream. \
         `DfpwmDecoder` exists only to refuse loudly with that finding \
         rather than emit audio nothing else agrees with (D6/D19).",
    ),
    (
        "eac3",
        "vaco-codec-ac3::eac3's own module doc: reachable only behind the \
         non-default `patent-unverified-eac3-decode` feature. D9's legal \
         register: E-AC-3's last-patent-expiry claim rests on a single \
         hedged secondary source, unconfirmed by counsel -- not shipped in \
         the default build regardless of implementation completeness.",
    ),
    (
        "adpcm_g722",
        "vaco-codec-adpcm's own fragment comment: 'the standardised ADPCM \
         subset (issue #280, C-02)' still does not implement G.722's \
         incompatible QMF/predictor algorithm; no existing ADPCM decoder \
         can be pointed at it instead.",
    ),
    (
        "aac_latm",
        "vaco-codec-aac's own fragment comment says the plain `aac` \
         decoder is 'registered even though vaco-codec-aac does not yet \
         decode a single [frame]' (patent-encumbered-aac-decode, epic \
         #53). LATM/LOAS re-frames the identical raw_data_block AAC \
         payload `vaco-parse-aac::PARSER_LATM` already extracts, so there \
         is nothing for a second, LATM-specific decoder to do that the \
         still-incomplete plain AAC decoder does not already need to do \
         first.",
    ),
    (
        "mpeg4",
        "MPEG-4 Part 2 (ISO/IEC 14496-2) pixel decode has no crate at all \
         in this tree yet -- `vaco-parse-mpegvideo::mpeg4` only extracts \
         VOL/VOP header fields, unlike its MPEG-1/2 sibling which has a \
         real decoder in vaco-codec-mpeg12. Tracked by the open D-22/T2-02 \
         shared-MPEG-family-decoder epics.",
    ),
    (
        "truehd",
        "T5-01 (issue #453, ~120pw at a 2.5x clean-room multiplier): TrueHD/MLP \
         is one of the ~15 high-value spec-less formats the two-team \
         clean-room programme covers. No crate in this tree decodes it yet.",
    ),
    (
        "wavpack",
        "Same T5-01/#453 programme as `truehd` above; no crate decodes it yet.",
    ),
    (
        "tta",
        "Same T5-01/#453 programme as `truehd` above; no crate decodes it yet.",
    ),
    (
        "dts",
        "T3-06 (open epic): DTS core decode is not implemented anywhere in \
         this tree yet. Also named in T5-01/#453's spec-less list. \
         vaco-demux-matroska (and any other container carrying DTS) \
         reports the stream honestly -- codec_id, sample_rate, channels -- \
         and leaves sample_fmt as `unknown` rather than a decoded-format \
         guess with nothing behind it (D9's registered-but-wrong-is-worse-\
         than-absent rule).",
    ),
    (
        "binkaudio_dct",
        "Bink is named in T5-01/#453's spec-less-format list; no crate in \
         this tree decodes any part of it (vaco-format-misc::bink only \
         demuxes the container).",
    ),
    (
        "binkaudio_rdft",
        "Same Bink/#453 scope as `binkaudio_dct` above.",
    ),
    (
        "binkvideo",
        "Same Bink/#453 scope as `binkaudio_dct` above.",
    ),
    (
        "smackaudio",
        "Smacker is named in T5-01/#453's spec-less-format list; no crate \
         in this tree decodes any part of it (vaco-format-misc::smk only \
         demuxes the container).",
    ),
    (
        "smackvideo",
        "Same Smacker/#453 scope as `smackaudio` above.",
    ),
    (
        "wmav1",
        "WMA v1/v2 is named in T5-01/#453's spec-less-format list; no \
         crate in this tree decodes it yet.",
    ),
    ("wmav2", "Same WMA/#453 scope as `wmav1` above."),
    (
        "wmapro",
        "Same WMA/#453 scope as `wmav1` above, for the WMA Professional \
         variant.",
    ),
    (
        "huffyuv",
        "HuffYUV/FFVHuff is named in T5-01/#453's spec-less-format list; \
         no crate in this tree decodes it yet (vaco-format-riff only names \
         the FourCC).",
    ),
    (
        "jacosub",
        "vaco-codec-subtitle-text is a real, deliberately unregistered \
         decoder crate -- see rule A's own ALLOW_ORPHAN_CRATE entry: its \
         own module doc says wiring is 'a small, mechanical follow-up' not \
         done because `vaco_frame::FrameData::Subtitle` was uncommitted \
         work in another agent's tree when it was written. Covers every \
         plain-text subtitle format `vaco-subtitle-text` demuxes: this \
         entry and the nine below it are one gap, not ten.",
    ),
    (
        "microdvd",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "mpl2",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "pjs",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "realtext",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "sami",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "stl",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "subviewer",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "subviewer1",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "vplayer",
        "Same vaco-codec-subtitle-text scope as `jacosub` above.",
    ),
    (
        "scte_35",
        "MediaType::Data: SCTE-35 splice cues are opaque metadata carried \
         through packets, not decoded into frames -- the reference itself \
         has no decoder named `scte_35` either (checked against \
         xtask/data/reference-formats.txt). Structurally decoder-less, \
         not an implementation gap.",
    ),
    (
        "timed_id3",
        "Same MediaType::Data reasoning as `scte_35` above: timed ID3 is \
         metadata read directly, and the reference has no decoder for it.",
    ),
    (
        "klv",
        "Same MediaType::Data reasoning as `scte_35` above: SMPTE 336M \
         KLV is metadata read directly, and the reference has no decoder \
         for it.",
    ),
    (
        "bin_data",
        "MediaType::Data: the reference's own pseudo-codec for a stream \
         it has been told nothing about beyond 'this is data' (MPEG-TS \
         stream_type 0x05/0x06 with no descriptor) -- checked directly, \
         `ffmpeg -h decoder=bin_data` reports 'known to FFmpeg, but no \
         decoders for it are available'. Same MediaType::Data reasoning \
         as `scte_35` above, not a gap this build is behind on.",
    ),
    (
        "vvc",
        "T3-07 (open epic, issue #452): VVC decode is not implemented \
         anywhere in this tree yet.",
    ),
    (
        "avs2",
        "No crate in this tree decodes AVS2 yet; only the CodecId variant \
         and container-level mapping exist (finding 4, vaco-demux-matroska).",
    ),
    ("avs3", "Same scope as `avs2` above, for AVS3."),
    (
        "cavs",
        "Same scope as `avs2` above, for the earlier Chinese AVS \
         (CodecId::Cavs) generation.",
    ),
    (
        "dirac",
        "T2-11 (open epic): Dirac/VC-2 decode is not implemented anywhere \
         in this tree yet.",
    ),
    (
        "evc",
        "No crate in this tree decodes MPEG-5 EVC yet; not yet scoped as \
         its own tracked epic.",
    ),
    (
        "jpeg2000",
        "T2-07 (open epic): JPEG 2000 decode is not implemented anywhere \
         in this tree yet.",
    ),
    (
        "dvvideo",
        "T2-06 (open epic): DV decode is not implemented anywhere in this \
         tree yet (vaco-format-dv only demuxes the container/profile).",
    ),
    (
        "dnxhd",
        "T2-09c (open epic): DNxHD/VC-3 decode is not implemented anywhere \
         in this tree yet.",
    ),
    (
        "msmpeg4v3",
        "Tracked by the open D-22 shared-MPEG-family-decoder epic \
         (H.261/H.263/MPEG-1/2/4/MSMPEG4/WMV1/2/FLV1/RV10/20); no crate \
         decodes MSMPEG4v3 specifically yet.",
    ),
    (
        "flv1",
        "Same D-22 shared-MPEG-family-decoder scope as `msmpeg4v3` above.",
    ),
    (
        "vp6",
        "No crate in this tree decodes VP6 (Sorenson/On2) yet; not yet \
         scoped as its own tracked epic.",
    ),
    (
        "vp6a",
        "Same scope as `vp6` above, for the alpha-channel variant.",
    ),
    ("vp6f", "Same scope as `vp6` above, for the Flash variant."),
    (
        "flashsv",
        "No crate in this tree decodes Flash Screen Video yet; not yet \
         scoped as its own tracked epic.",
    ),
    ("flashsv2", "Same scope as `flashsv` above, for version 2."),
    (
        "flic",
        "No crate in this tree decodes Autodesk FLIC/FLC yet \
         (vaco-format-misc::flic only demuxes the container).",
    ),
    (
        "cdgraphics",
        "No crate in this tree decodes CD+Graphics yet \
         (vaco-format-misc::cdg only demuxes the container).",
    ),
    (
        "roq",
        "No crate in this tree decodes id RoQ video yet \
         (vaco-format-misc::roq only demuxes the container).",
    ),
    (
        "roq_dpcm",
        "Same scope as `roq` above, for its DPCM audio half.",
    ),
    (
        "cljr",
        "No crate in this tree decodes Cirrus Logic AccuPak yet; not yet \
         scoped as its own tracked epic.",
    ),
    (
        "nellymoser",
        "No crate in this tree decodes Nellymoser ASAO yet \
         (vaco-demux-flv only names the FourCC).",
    ),
    (
        "gsm",
        "No crate in this tree decodes GSM 06.10 yet; only \
         vaco-format-rtp's RTP depacketiser names the CodecId.",
    ),
    (
        "gsm_ms",
        "Same scope as `gsm` above, for the Microsoft framing variant.",
    ),
    (
        "amr_nb",
        "No crate in this tree decodes AMR-NB yet; only container-level \
         detection (vaco-format-misc-audio) and RTP depacketisation name \
         the CodecId.",
    ),
    ("amr_wb", "Same scope as `amr_nb` above, for AMR-WB."),
    (
        "ilbc",
        "No crate in this tree decodes iLBC yet; only vaco-format-rtp's \
         RTP depacketiser names the CodecId.",
    ),
    (
        "qcelp",
        "No crate in this tree decodes QCELP yet; only vaco-format-rtp's \
         RTP depacketiser names the CodecId.",
    ),
    (
        "g723_1",
        "No crate in this tree decodes G.723.1 yet \
         (vaco-format-misc-audio only demuxes the container).",
    ),
    (
        "g728",
        "No crate in this tree decodes G.728 yet; only container-level \
         detection and RTP depacketisation name the CodecId.",
    ),
    (
        "g729",
        "No crate in this tree decodes G.729 yet; only container-level \
         detection and RTP depacketisation name the CodecId.",
    ),
    (
        "speex",
        "No crate in this tree decodes Speex yet (vaco-demux-ogg/\
         vaco-mux-ogg and vaco-format-rtp only name the CodecId).",
    ),
    (
        "sbc",
        "No crate in this tree decodes Bluetooth SBC yet \
         (vaco-format-misc-audio only demuxes the container).",
    ),
    (
        "aptx",
        "No crate in this tree decodes aptX yet \
         (vaco-format-misc-audio only demuxes the container).",
    ),
    ("aptx_hd", "Same scope as `aptx` above, for aptX HD."),
    (
        "adpcm_adx",
        "No crate in this tree decodes CRI ADX ADPCM yet \
         (vaco-format-misc-audio only demuxes the container) -- a \
         genuinely different algorithm from the four ADPCM variants \
         vaco-codec-adpcm implements, not a resolvable-elsewhere gap.",
    ),
];

/// Every `CodecId` some demuxer in the tree can construct, checked against
/// the registered decoder table -- rule G1 asks "is there a demuxer for
/// this decoder's codec?"; this asks it the other way round: "is there a
/// decoder for what this demuxer can produce?" Neither implies the other —
/// a registry can be entirely self-consistent by G1's measure and still
/// contain a demuxer whose own output nothing can decode.
///
/// The PCM incident's exact shape: `vaco-demux-matroska` could resolve an
/// `A_PCM/*` track to the generic `CodecId::Pcm` -- a real, spellable
/// variant with no registered decoder anywhere in this tree, only its 21
/// concrete `PcmS16le`/`PcmF64le`/etc. siblings have one -- consistent with
/// every rule above, and completely unusable: `vaco -i pcm.mka -f s16le
/// out.raw` failed outright with "this build has no decoder for the input
/// codec". Nothing at registration time asked this question; the gap
/// surfaced only at runtime, on a real file.
///
/// A codec this finds needs one of two responses, and they are not
/// interchangeable:
/// - **The demuxer's own resolution could pick a concrete, decodable
///   variant and currently does not** — fix the demuxer. This is what the
///   `CodecId::Pcm` case actually was, and [`ALLOW_UNDECODABLE_PRODUCED`]
///   must never be used to paper over it.
/// - **Nothing in this tree decodes the concrete codec yet** — a real,
///   declared gap. Add a row to `ALLOW_UNDECODABLE_PRODUCED` naming why,
///   the same discipline `dup-check`'s `DISTINCT` and `owner-gate`'s
///   `MEDIA` already apply.
fn check_decoder_exists_for_produced_codecs(
    rows: &[Row],
    variant_to_name: &Map<String, String>,
) -> Vec<String> {
    let all = crates();
    let demuxer_crates: Set<String> = rows
        .iter()
        .filter(|r| r.kind == "demuxer")
        .map(|r| r.krate.clone())
        .collect();

    let mut universe: Set<String> = Set::new();
    for krate in &demuxer_crates {
        universe.extend(transitive_crate_closure(krate, &all));
    }
    let producible = codecs_referenced_in(&universe, variant_to_name);

    let decodable: Set<String> = rows
        .iter()
        .filter(|r| r.kind == "decoder")
        .filter_map(|r| r.codec.clone())
        .collect();

    let mut violations = Vec::new();
    for codec in &producible {
        if decodable.contains(codec)
            || ALLOW_UNDECODABLE_PRODUCED
                .iter()
                .any(|(n, _)| *n == codec.as_str())
        {
            continue;
        }
        violations.push(format!(
            "  `{codec}` is a `CodecId` some demuxer in the tree can construct, \
             but no registered decoder anywhere handles it — the PCM \
             incident's shape. Either the demuxer's own resolution should \
             pick a concrete, decodable variant instead of a generic \
             fallback, or this is a genuine gap that needs a row in \
             ALLOW_UNDECODABLE_PRODUCED naming why `{codec}` is not \
             decodable yet.",
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
    ("subtitles", "filter: same as `ass` above — needs libass."),
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
    (
        "vp9_extract_vpcc",
        "bitstream_filter: has no reference equivalent by design, not by \
         omission — the reference's own VP9 bitstream filters are exactly \
         `vp9_metadata`, `vp9_raw_reorder`, `vp9_superframe`, \
         `vp9_superframe_split` (`vaco-bsf-vpx`'s own module doc measured \
         this against `ffmpeg -bsfs`), none of which derive a `vpcC` \
         configuration record from frame headers. This filter exists to \
         close a vaco-specific gap `ffmpeg` never had the same way: its MP4 \
         muxer derives a `vp09` sample entry's `vpcC` from its own decoder's \
         internal state on the fly, with no bitstream-filter seam exposed \
         for it at all — this workspace instead has to make that derivation \
         an explicit, named, registry-visible step (D14.1: a mux crate \
         cannot depend on a parse crate directly). See \
         `vaco-bsf-vpx::extract_vpcc`'s own module doc for the bug this \
         closes (`vaco -c copy` of VP9-in-Matroska into MP4 producing an \
         empty `vpcC` box real `ffprobe` refuses to open).",
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

// ------------------------------------------------------------------ rule I

/// Every `#[opt(...)]` attribute span (byte range, inclusive of both
/// brackets) in `text`, found by brace/paren balancing from `#[opt(` —
/// the same technique [`function_body`] and rule G/H's scanners already
/// use in this file, not a Rust parser.
fn opt_attr_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while let Some(rel) = text.get(i..).and_then(|s| s.find("#[opt(")) {
        let start = i + rel;
        let mut depth = 0i32;
        let mut started = false;
        let mut j = start;
        while j < bytes.len() {
            match bytes[j] {
                b'(' => {
                    depth += 1;
                    started = true;
                }
                b')' => depth -= 1,
                _ => {}
            }
            if started && depth == 0 {
                break;
            }
            j += 1;
        }
        // advance to the attribute's closing `]`, if present nearby.
        let close = text[j..].find(']').map_or(j, |k| j + k);
        spans.push((start, close));
        i = close + 1;
    }
    spans
}

/// The field name an `#[opt(...)]` attribute at `attr_end` (its closing
/// `]`, from [`opt_attr_spans`]) applies to: the identifier before the
/// first `:` on the next non-blank, non-attribute, non-doc-comment line.
fn field_after_opt_attr(text: &str, attr_end: usize) -> Option<String> {
    let rest = &text[attr_end + 1..];
    for line in rest.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("///") || t.starts_with("//") {
            continue;
        }
        let t = t.strip_prefix("pub(crate)").unwrap_or(t);
        let t = t.strip_prefix("pub(super)").unwrap_or(t);
        let t = t.strip_prefix("pub").unwrap_or(t);
        let t = t.trim_start();
        let ident_end = t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
        if t[ident_end..].trim_start().starts_with(':') {
            return Some(t[..ident_end].to_owned());
        }
        return None;
    }
    None
}

/// `field` counts as read when `.field` appears anywhere in `text` outside
/// `exclude_start..exclude_end` (the attribute plus its own declaration
/// line) — a plain textual scan, the same shape rule G/H's
/// `codecs_referenced_in` already uses. [`check_unconsumed_options`] calls
/// this with `text` set to one whole crate's concatenated source, not one
/// file, and its own doc explains why: two `#[derive(Options)]` structs
/// sharing a field name can still make a genuinely dead field read as
/// "used" by a same-named field's real use, but that has only been observed
/// between *unrelated crates* (`RtspOptions::user_agent` versus
/// `vaco-protocol-http`/`vaco-protocol-icecast`'s own `user_agent` fields;
/// `FormatOptions::recursion_limit` versus `RemoteAccess::recursion_limit`)
/// in this tree, which crate scope already excludes — not within one crate,
/// which is what would defeat this specific choice of scope.
fn opt_field_is_read_elsewhere(
    text: &str,
    field: &str,
    exclude_start: usize,
    exclude_end: usize,
) -> bool {
    let pattern = format!(".{field}");
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find(&pattern) {
        let pos = search_from + rel;
        let after = pos + pattern.len();
        let boundary_ok = text[after..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        if boundary_ok && !(exclude_start <= pos && pos < exclude_end) {
            return true;
        }
        search_from = pos + 1;
    }
    false
}

/// `(field, file suffix, why)` for a field this gate would otherwise flag,
/// kept out of the report with a reason on file.
///
/// Empty today. The two shapes a reason here can take: a field this scan's
/// same-file scope cannot see is genuinely read (a same-name collision with
/// another struct in the same file, the kind this module's own doc
/// describes finding in `kerndeint`'s `map` and `misc.rs`'s `PermsOpts` —
/// both fixed, so neither needs an entry any more), or a field a future
/// pass finds and cannot fix in the same commit. The second shape is *not*
/// the discipline every other allowlist in this file holds to — those
/// record a deliberate, permanent divergence; an entry of this shape is an
/// open debt, named so the class stops growing while it is worked down, not
/// a claim that leaving it is fine.
const ALLOW_UNCONSUMED_OPTION: &[(&str, &str, &str)] = &[(
    "listen_timeout",
    "vaco-demux-rtsp/src/options.rs",
    "server-mode-only per the reference (\"imply flag listen\"); this crate is a \
     client only, so the field is accepted for interface parity, not silently \
     misapplied -- the field's own doc comment states this in full, the same \
     declared-gap shape as an `ALLOW_MUXER_ONLY`/`ALLOW_NAME_MISMATCH` row, not an \
     undisclosed one this rule exists to catch.",
)];

/// One crate's `src/` tree: every `.rs` file's path (repo-relative) and
/// text, concatenated once so a field's "is it read anywhere in this
/// crate" question is one substring scan rather than one per file pair.
struct CrateSrc {
    /// `(repo-relative path, file text, that file's start offset in
    /// `joined`)`, in the order [`rust_files`] returns.
    files: Vec<(String, String, usize)>,
    /// Every file's text concatenated in order, each preceded by a `\n` so
    /// no field name can straddle a file boundary and false-positive.
    joined: String,
}

/// Blank out (space- and newline-preserving, so byte offsets do not move)
/// every `#[cfg(test)]`/`#[test]`-guarded item in `text`.
///
/// Shared by two different scans that both learned this the hard way.
/// [`check_unconsumed_options`] (rule I) first needed it because
/// `vaco-format-core::FormatOptions`'s `rtbufsize`/`max_delay`/`err_detect`
/// each had exactly one `.field` occurrence in their own crate outside
/// their own declaration, and every one was `assert_eq!(o.field, <parsed
/// value>)` inside `#[cfg(test)] mod tests` — a test that a string parses
/// into the right field value, not a claim anything downstream reads it.
/// Left unmasked, that scan counted it as consumption and missed exactly
/// the shape of bug it exists to catch: nothing outside the
/// option-parsing layer itself ever looked at the field (since fixed —
/// see `vaco-format-core/src/options.rs`). [`codecs_referenced_in`] (rule
/// G) uses it for the identical reason, one call site over: a `CodecId`
/// named only inside a test helper is not proof any real demuxer/muxer
/// constructs it. Re-running rule G with masking in place found nothing
/// new in this tree today, but the fix stays — a scan that happens to be
/// right today by luck is not the same claim as one that is right by
/// construction, and the next crate to add a test-only `CodecId` mention
/// should not get to repeat rule I's original mistake.
fn mask_test_code(text: &str) -> String {
    let mut out: Vec<u8> = text.as_bytes().to_vec();
    for guard in ["#[cfg(test)]", "#[test]"] {
        let mut i = 0;
        while let Some(rel) = std::str::from_utf8(&out[i..])
            .ok()
            .and_then(|s| s.find(guard))
        {
            let start = i + rel;
            let Some(brace_rel) = std::str::from_utf8(&out[start..])
                .ok()
                .and_then(|s| s.find('{'))
            else {
                i = start + guard.len();
                continue;
            };
            let brace = start + brace_rel;
            let mut depth = 0i32;
            let mut end = brace;
            for (k, &b) in out.iter().enumerate().skip(brace) {
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for b in &mut out[start..=end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
            i = end + 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn crate_src(src: &std::path::Path) -> CrateSrc {
    let mut files = Vec::new();
    let mut joined = String::new();
    for file in rust_files(src) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(repo_root())
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        joined.push('\n');
        let offset = joined.len();
        joined.push_str(&mask_test_code(&text));
        files.push((rel, text, offset));
    }
    CrateSrc { files, joined }
}

/// A registered component crate's own `#[opt(...)]` fields, checked against
/// every file in the *same crate* — not just the field's own file, and not
/// the whole workspace.
///
/// Same-file-only was tried first and over-fired: `vaco-format-core`'s
/// `FormatOptions` and `vaco-demux-rtsp`'s `RtspOptions` both declare their
/// fields in one `options.rs` and are read from sibling files in the same
/// crate (`interleave.rs`, `mux.rs`, `discovery.rs`, `time.rs`) — the
/// ordinary shape for a config struct with a dedicated declaration module,
/// not a bug. Workspace-wide was tried and tried to correct that, and
/// silently broke in the other direction: an unrelated crate's *own*
/// `user_agent` or `recursion_limit` field (`vaco-protocol-http`,
/// `vaco-protocol-icecast`, `vaco-format-adaptive::RemoteAccess` all have
/// one) reads as "used" for `RtspOptions::user_agent` and
/// `FormatOptions::recursion_limit`, which are not the same field and were
/// not actually read anywhere — a real bug in each case (see
/// `vaco-demux-rtsp/src/options.rs` and `vaco-format-core/src/options.rs`'s
/// own fix commits), hidden by a coincidental name match one layer further
/// away than same-file scope reaches. Crate scope is the middle ground:
/// wide enough to see a config struct's own consumers in sibling files,
/// narrow enough that an unrelated crate's same-named field cannot vouch
/// for this one.
///
/// This does not make same-crate, different-*file* collisions impossible in
/// principle — the same risk [`opt_field_is_read_elsewhere`]'s doc names for
/// same-file structs could in theory recur across two files in one crate —
/// but no instance of it has been found in this tree, unlike the two shapes
/// above, both of which were.
fn check_unconsumed_options() -> Result<Vec<String>, String> {
    let mut violations = Vec::new();
    for base in ["crates/filter", "crates/codec", "crates/format"] {
        let root = repo_root().join(base);
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for crate_dir in entries.flatten() {
            let src = crate_dir.path().join("src");
            if !src.is_dir() {
                continue;
            }
            let krate = crate_src(&src);
            for (rel, text, base_offset) in &krate.files {
                if !text.contains("#[opt(") {
                    continue;
                }
                for (start, end) in opt_attr_spans(text) {
                    let Some(field) = field_after_opt_attr(text, end) else {
                        continue;
                    };
                    // Exclude the attribute itself and the field's own
                    // declaration line from the "read elsewhere" search —
                    // otherwise every field would trivially read as used by
                    // its own `#[opt(name = "...")]`/`pub field: T,`, this
                    // time as offsets into the whole-crate `joined` text.
                    let decl_line_end = text[end..].find(',').map_or(text.len(), |k| end + k + 1);
                    let excl_start = base_offset + start;
                    let excl_end = base_offset + decl_line_end;
                    if opt_field_is_read_elsewhere(&krate.joined, &field, excl_start, excl_end) {
                        continue;
                    }
                    if ALLOW_UNCONSUMED_OPTION
                        .iter()
                        .any(|(f, fl, _)| *f == field && rel.ends_with(fl))
                    {
                        continue;
                    }
                    violations.push(format!(
                        "  {rel}: field `{field}` is declared with `#[opt(...)]` but never \
                         read anywhere in this crate — parsing it has no effect on output. \
                         This is the CLI's `-filter_threads` shape of bug, one level down: \
                         implement it, make a non-default value refuse by name (`cargo xtask \
                         reachability-check`'s rule I is what caught the \
                         `vaco-filter-deinterlace`/`vaco-format-core`/`vaco-demux-rtsp` batch \
                         this way), or add it to ALLOW_UNCONSUMED_OPTION with a reason if an \
                         unrelated crate's same-named field is the only reason this looked \
                         used."
                    ));
                }
            }
        }
    }
    violations.sort();
    Ok(violations)
}

// ------------------------------------------------------------------ rule K

/// Every `vaco-bsf-*` filter this tree has, and why nothing's `check_bitstream`
/// requests it automatically.
///
/// Found by the bitstream-filter reachability sweep the `aac_adtstoasc`
/// incident (a real, correct filter fixing a live conformance failure once
/// requested — see `crates/format/vaco-mux-matroska/src/mux.rs`) motivated: if
/// one filter could sit complete and orphaned, the odds it was the only one
/// were poor. It was not the only one, but most of the rest turned out to
/// belong here rather than needing the same fix — this is where that
/// distinction is written down so it does not have to be re-derived.
///
/// Three real shapes hide behind "not requested by any muxer's
/// `check_bitstream`", and only the first is a bug:
///
/// - **Orphaned.** Exists, correct, requested by nothing, and something in
///   this tree needs it. `aac_adtstoasc` was this; fixed, not listed here.
/// - **Measured manual-only.** Every `*_metadata` rewriter (`h264_metadata`,
///   `hevc_metadata`, `av1_metadata`, `vp9_metadata`, `mpeg2_metadata`,
///   `prores_metadata`, `opus_metadata`) is an *identity transform* by
///   default in real ffmpeg — every option defaults to "leave the bitstream
///   alone" (each filter's own module doc measures this directly) — and this
///   tree cannot even pass per-instance bsf options yet
///   (`planning/INTERFACE-GAPS.md` gap 12), so auto-inserting one today would
///   do nothing every single call. `vaco-bsf-generic`'s debug/utility family
///   (`chomp`, `dump_extra`, `filter_units`, `showinfo`, `setts`, `null`,
///   `remove_extra`, `trace_headers`, `noise`) is the same shape real ffmpeg
///   ships them as: inspection and stream-editing tools a person reaches for
///   by name, never something a muxer would ask for on its own behalf.
///   `pcm_rechunk` is real ffmpeg's own manual re-slicer, not a muxer
///   requirement — the one place this tree needed fixed-size PCM chunking
///   (MP4's 1024-sample grouping) was solved directly in the demuxer instead.
///   `vp9_superframe`/`vp9_superframe_split` were *tested* as an orphaned bug
///   exactly like `aac_adtstoasc` and reverted: measured directly against
///   real ffmpeg 9.0.1 (`vp9_altref_invisible_frames.ivf`, remuxed through a
///   real MP4 and on into Matroska with `-c copy`, 10 of 125 packets
///   confirmed carrying a genuine superframe index), packet sizes come out
///   identical on both sides — no split happens, because a VP9 decoder reads
///   the superframe index itself regardless of what the container did to
///   deliver the bytes. `av1_frame_split`/`av1_frame_merge` are the AV1
///   analogue, measured the same way in their own module docs against real
///   ffmpeg's `-bsf:v` directly (not against an in-tree muxer, since none in
///   this tree currently produces or consumes multi-frame-per-packet AV1).
/// - **Blocked on a bigger gap.** `text2movsub`/`mov2textsub` wrap and unwrap
///   MP4's `mov_text` sample format, but no muxer or demuxer in this tree
///   supports an MP4 subtitle track at all yet — there is no container-level
///   feature for either direction of this filter to hook into, automatic or
///   manual, until that lands.
///
/// None of the second or third group has a *manual* path either: this tree
/// has no `-bsf` CLI flag (unlike real ffmpeg's `-bsf:v`/`-bsf:a`/`-bsf:s`) —
/// only `-bsfs`/`-h bsf=<name>` listing/introspection exist. A filter
/// recorded here as "manual-only" is therefore unreachable in the shipped
/// binary today exactly like an orphaned one would be; the distinction this
/// rule tracks is *intent* (should something request this, automatically or
/// by a CLI flag that does not exist yet) so the next person does not have to
/// re-derive it, not present-day reachability, which rule K already tests
/// directly.
const ALLOW_MANUAL_ONLY_BSF: &[(&str, &str)] = &[
    (
        "opus_metadata",
        "identity transform by default (own module doc, measured against \
         real ffmpeg); this tree cannot pass per-instance bsf options yet \
         (INTERFACE-GAPS.md gap 12), so there is nothing for an automatic \
         insertion to do.",
    ),
    (
        "av1_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "h264_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "hevc_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "mpeg2_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "prores_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "vp9_metadata",
        "same reasoning as opus_metadata above: identity by default, gap 12 \
         blocks any option ever reaching it.",
    ),
    (
        "pcm_rechunk",
        "real ffmpeg's own manual re-slicer, not something a muxer requires; \
         the one in-tree need for fixed-size PCM chunking (MP4's 1024-sample \
         grouping) was solved directly in the demuxer, not through this bsf.",
    ),
    (
        "chomp",
        "vaco-bsf-generic's debug/utility family, the same shape real ffmpeg \
         ships them as: a person reaches for these by name, no muxer ever \
         asks for one on its own behalf.",
    ),
    ("dump_extra", "same reasoning as chomp above."),
    ("filter_units", "same reasoning as chomp above."),
    ("showinfo", "same reasoning as chomp above."),
    ("setts", "same reasoning as chomp above."),
    ("null", "same reasoning as chomp above."),
    ("remove_extra", "same reasoning as chomp above."),
    ("trace_headers", "same reasoning as chomp above."),
    ("noise", "same reasoning as chomp above."),
    (
        "vp9_superframe",
        "tested as an orphaned bug and reverted: measured directly against \
         real ffmpeg 9.0.1 (vp9_altref_invisible_frames.ivf through a real \
         MP4 and on into Matroska with `-c copy`, 10 of 125 packets confirmed \
         carrying a genuine superframe index) shows identical packet sizes on \
         both sides -- no merge happens either. See \
         crates/format/vaco-mux-matroska/src/mux.rs's check_bitstream doc.",
    ),
    (
        "vp9_superframe_split",
        "tested as an orphaned bug and reverted; see vp9_superframe's entry \
         above for the measurement (both filters share it, split and merge \
         being inverses).",
    ),
    (
        "av1_frame_split",
        "measured manual-only in its own module doc, against real ffmpeg's \
         `-bsf:v` directly rather than any in-tree muxer -- no muxer or \
         demuxer in this tree currently produces or consumes \
         multi-frame-per-packet AV1 for an automatic insertion to fix.",
    ),
    (
        "av1_frame_merge",
        "same reasoning as av1_frame_split above (its own module doc measures \
         the inverse direction the same way).",
    ),
    (
        "text2movsub",
        "blocked on a bigger gap, not manual-only by choice: no muxer or \
         demuxer in this tree supports an MP4 subtitle track at all yet, so \
         there is no container-level feature for this filter to wrap output \
         for, automatic or manual, until one exists.",
    ),
    (
        "mov2textsub",
        "same gap as text2movsub above (the unwrap direction of the same \
         missing MP4-subtitle-track support).",
    ),
];

/// A `BitstreamAction::Insert { name: "..." }` (or a `match` arm feeding one)
/// anywhere in this tree's source is a real request for that filter by name —
/// this collects every string literal that appears within such a call's own
/// braces, which is a deliberate over-approximation for a `match`-typed name
/// (`raw.rs`'s own `check_bitstream` picks `h264_mp4toannexb` or
/// `hevc_mp4toannexb` depending on `codec_id`; both literals sit inside the
/// same `Insert { ... }` span and both are real, reachable requests, just not
/// on the same call).
fn requested_bsf_names(text: &str) -> Set<String> {
    let mut out = Set::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find("BitstreamAction::Insert") {
        let start = i + rel;
        let Some(brace_rel) = text[start..].find('{') else {
            break;
        };
        let brace = start + brace_rel;
        let mut depth = 0i32;
        let mut end = brace;
        for (k, &b) in bytes.iter().enumerate().skip(brace) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = k;
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(span) = text.get(brace..=end) {
            let mut rest = span;
            while let Some(q1) = rest.find('"') {
                let Some(q2) = rest[q1 + 1..].find('"') else {
                    break;
                };
                out.insert(rest[q1 + 1..q1 + 1 + q2].to_owned());
                rest = &rest[q1 + 1 + q2 + 1..];
            }
        }
        i = end.max(start + 1);
    }
    out
}

/// A registered `bitstream_filter` component whose `name` no
/// `BitstreamAction::Insert` anywhere in this tree ever requests, and is not
/// in [`ALLOW_MANUAL_ONLY_BSF`] with a reason on record.
///
/// This is `check_bsf_chaining`'s (rule C) question asked one layer further
/// on: rule C asks whether `Bsfs::open` can construct a filter *by name*
/// (registry consistency); this asks whether anything in the tree ever
/// *supplies* that name to open it (reachability). Neither implies the
/// other — `aac_adtstoasc` passed rule C the whole time.
fn check_bsf_reachable(rows: &[Row]) -> Result<Vec<String>, String> {
    let mut requested: Set<String> = Set::new();
    for (_area, _name, path) in crates() {
        for file in rust_files(&path.join("src")) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let masked = mask_test_code(&text);
            requested.extend(requested_bsf_names(&masked));
        }
    }

    let mut violations = Vec::new();
    for r in rows.iter().filter(|r| r.kind == "bitstream_filter") {
        for name in &r.names {
            if requested.contains(name) {
                continue;
            }
            if ALLOW_MANUAL_ONLY_BSF.iter().any(|(n, _)| n == name) {
                continue;
            }
            violations.push(format!(
                "  {name} ({}) is a registered bitstream_filter component that \
                 no `BitstreamAction::Insert` anywhere in this tree ever \
                 requests, and it is not in ALLOW_MANUAL_ONLY_BSF with a \
                 reason on record. Either wire it into some muxer's \
                 `check_bitstream` (the aac_adtstoasc shape), or add it to \
                 ALLOW_MANUAL_ONLY_BSF explaining why nothing should — \
                 measured against real ffmpeg first, the way vp9_superframe's \
                 own entry there had to be corrected after the fact.",
                r.krate
            ));
        }
    }
    violations.sort();
    Ok(violations)
}

// ------------------------------------------------------------------- driver

pub fn run(_check: bool) -> Task {
    let rows = all_rows()?;
    let variant_to_name = codec_name_table()?;

    let sections: [(&str, Vec<String>); 12] = [
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
        (
            "D. muxer with no demuxer of the same name",
            check_muxer_only(&rows),
        ),
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
        (
            "I. declared #[opt(...)] field never read in its own file",
            check_unconsumed_options()?,
        ),
        (
            "J. demuxer-producible codec with no registered decoder",
            check_decoder_exists_for_produced_codecs(&rows, &variant_to_name),
        ),
        (
            "K. registered bitstream filter no BitstreamAction::Insert ever requests",
            check_bsf_reachable(&rows)?,
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
        + ALLOW_NAME_MISMATCH.len()
        + ALLOW_UNCONSUMED_OPTION.len()
        + ALLOW_MANUAL_ONLY_BSF.len();
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

    #[test]
    fn every_undecodable_produced_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_UNDECODABLE_PRODUCED {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn undecodable_produced_allowlist_has_no_duplicate_codec_names() {
        let mut names: Vec<&str> = ALLOW_UNDECODABLE_PRODUCED.iter().map(|(n, _)| *n).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            before,
            "a duplicate row hides which entry a reader should trust"
        );
    }

    /// Rule J itself, run against the real tree: every `CodecId` some
    /// demuxer in the workspace can construct must either have a
    /// registered decoder or a named row in `ALLOW_UNDECODABLE_PRODUCED`.
    /// This is what would have caught the `CodecId::Pcm` incident before a
    /// user hit it on a real file, and it is the rule's own regression
    /// test: a codec someone wires into a demuxer later without a decoder
    /// (or without declaring the gap) fails this, not just `cargo xtask
    /// reachability-check` run by hand.
    #[test]
    fn check_decoder_exists_for_produced_codecs_is_clean_against_the_real_tree() {
        let rows = all_rows().expect("rows parse against the real tree");
        let variant_to_name = codec_name_table().expect("vaco-codec-core's CODECS table parses");
        let violations = check_decoder_exists_for_produced_codecs(&rows, &variant_to_name);
        assert!(
            violations.is_empty(),
            "rule J found {} undeclared undecodable-produced codec(s):\n{}",
            violations.len(),
            violations.join("\n")
        );
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
        assert!(
            table.len() > 50,
            "expected dozens of codecs, got {}",
            table.len()
        );
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
        assert!(
            !demux.contains("mov,mp4,m4a"),
            "must split, not keep the joined line"
        );
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

    #[test]
    fn every_unconsumed_option_allowlist_row_has_a_real_reason() {
        for (field, _file, why) in ALLOW_UNCONSUMED_OPTION {
            assert!(why.len() > 15, "{field} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn opt_attr_spans_finds_one_multi_line_attribute() {
        let text = "#[opt(name = \"w\", help = \"width\",\n      default = 0, range = 0..=8192)]\npub width: i32,\n";
        let spans = opt_attr_spans(text);
        assert_eq!(spans.len(), 1);
        let (start, end) = spans[0];
        assert!(text[start..end].starts_with("#[opt("));
    }

    #[test]
    fn field_after_opt_attr_skips_pub_and_finds_the_identifier() {
        let text = "#[opt(name = \"w\")]\npub width: i32,\n";
        let end = opt_attr_spans(text)[0].1;
        assert_eq!(field_after_opt_attr(text, end).as_deref(), Some("width"));
    }

    #[test]
    fn opt_field_is_read_elsewhere_requires_a_word_boundary() {
        // `.thresh` must not match inside `.threshold` — a same-prefix field
        // in the same file is exactly the false-positive this check must not
        // produce (the mirror image of the collision false negative rule I's
        // own doc names).
        let text = "let x = opts.threshold;\n";
        assert!(!opt_field_is_read_elsewhere(text, "thresh", 0, 0));
        let text2 = "let x = opts.thresh;\n";
        assert!(opt_field_is_read_elsewhere(text2, "thresh", 0, 0));
    }

    #[test]
    fn opt_field_is_read_elsewhere_excludes_its_own_declaration() {
        let text = "#[opt(name = \"x\", default = 0)]\npub thresh: i32,\n";
        let (start, end) = opt_attr_spans(text)[0];
        let decl_end = text[end..].find(',').map_or(text.len(), |k| end + k + 1);
        assert!(!opt_field_is_read_elsewhere(
            text, "thresh", start, decl_end
        ));
    }

    #[test]
    fn check_unconsumed_options_is_clean_against_the_real_tree() {
        let violations = check_unconsumed_options().expect("scan runs");
        // Not asserting a specific count here (it would make this test
        // fragile against every future fix), just that the scan runs clean
        // against the real tree with today's ALLOW_UNCONSUMED_OPTION, the
        // same shape every other rule's test in this module uses.
        assert!(
            violations.is_empty(),
            "rule I found unconsumed options with no allowlist entry:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn every_manual_only_bsf_allowlist_row_has_a_real_reason() {
        for (name, why) in ALLOW_MANUAL_ONLY_BSF {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }

    #[test]
    fn manual_only_bsf_allowlist_has_no_duplicate_names() {
        let mut names: Vec<&str> = ALLOW_MANUAL_ONLY_BSF.iter().map(|(n, _)| *n).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "a duplicate name hides its sibling's reason"
        );
    }

    #[test]
    fn requested_bsf_names_finds_a_simple_request() {
        let text = "Ok(BitstreamAction::Insert {\n    name: \"aac_adtstoasc\",\n})\n";
        let names = requested_bsf_names(text);
        assert!(names.contains("aac_adtstoasc"));
    }

    #[test]
    fn requested_bsf_names_finds_every_literal_in_a_match_arm() {
        // raw.rs/asf/mux.rs/mpegts/mux.rs's own shape: the requested name
        // depends on `codec_id`, so both literals sit inside one `Insert {
        // ... }` span. Over-approximating (both count as "requested") is the
        // right call here, not a bug in the scan -- see this function's own
        // doc.
        let text = "Ok(BitstreamAction::Insert {\n    name: match params.codec_id {\n        Some(CodecId::Hevc) => \"hevc_mp4toannexb\",\n        _ => \"h264_mp4toannexb\",\n    },\n})\n";
        let names = requested_bsf_names(text);
        assert!(names.contains("hevc_mp4toannexb"));
        assert!(names.contains("h264_mp4toannexb"));
    }

    #[test]
    fn requested_bsf_names_ignores_a_bare_mention_outside_any_insert_braces() {
        let text = "// aac_adtstoasc used to be unrequested\n";
        assert!(requested_bsf_names(text).is_empty());
    }

    #[test]
    fn check_bsf_reachable_is_clean_against_the_real_tree() {
        let rows = all_rows().expect("fragments parse");
        let violations = check_bsf_reachable(&rows).expect("scan runs");
        // Same shape as check_unconsumed_options_is_clean_against_the_real_tree
        // above: not a specific count, just clean against today's
        // ALLOW_MANUAL_ONLY_BSF.
        assert!(
            violations.is_empty(),
            "rule K found a registered bitstream filter with no requester and \
             no allowlist entry:\n{}",
            violations.join("\n")
        );
    }
}
