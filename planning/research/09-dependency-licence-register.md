# 09 — Dependency Licence Register

> **READ ALONGSIDE DECISION D10.** This register's licence findings are accurate and remain the
> authority on what each crate is licensed under. Its *verdicts* predate D10 and should be re-read
> through its three gates: **pure Rust with zero FFI** (so every `-sys` and binding crate here is out,
> whatever its licence), **permissive licence** (so MPL-2.0 still excludes Symphonia and `mp4parse`),
> and **trusted and maintained** (assessed per crate at adoption). Everything previously marked
> "OPT-IN" on FFI grounds is now simply out; everything marked USE is a candidate subject to the
> maintenance gate. The open verification tasks in §9 stand.



Vetting of candidate Rust crates and C libraries against Vaco's policy (see `planning/00-decisions.md` D3).

**Policy recap.** Our code is MIT OR Apache-2.0. Dependencies are ALLOWED under MIT, MIT-0, Apache-2.0,
BSD-2-Clause, BSD-3-Clause, BSD-3-Clause-Clear, ISC, Zlib, 0BSD, CC0-1.0, Unicode-3.0.
DENIED: GPL-*, LGPL-*, AGPL-*, **MPL-2.0**, CDDL, SSPL, EPL, WTFPL, proprietary, unresolved.
Additionally, because we are writing pure safe Rust, any `*-sys` / FFI crate is **out of the default build**
regardless of its licence — those exist only behind non-default opt-in features.

Verdicts: **USE** = allowed and useful · **OPT-IN** = allowed licence but FFI/C, non-default feature only ·
**DENY** = policy violation · **REVIEW** = needs a manual entry before adoption.

---

## 1. Video codec libraries and bindings

| Item | Licence | Pure Rust? | Verdict | Note |
|---|---|---|---|---|
| dav1d (C) | BSD-2-Clause | no | OPT-IN | Deliberately permissive so it can be embedded anywhere. |
| `dav1d` crate (rust-av/dav1d-rs) | MIT | no (FFI) | OPT-IN | Safe wrapper over C dav1d. |
| `libdav1d-sys` | BSD-2-Clause | no | REVIEW | crates.io repo field points into `njaard/libavif-rs`, not a dedicated repo — confirm attribution. |
| rav1e | BSD-2-Clause | **yes** | USE | Pure Rust AV1 encoder. The single most important permissive precedent for us. |
| libaom (C) | BSD-2-Clause **+ AOM Patent Licence 1.0** | no | OPT-IN | Two files, both apply — check LICENSE *and* PATENTS together. |
| `aom-sys` | MIT | no | OPT-IN | No separate `libaom-sys` crate exists — treat any such reference as this crate or nonexistent. |
| SVT-AV1 (C) | BSD-3-Clause-Clear + AOM Patent Licence 1.0 | no | OPT-IN | Same dual-grant pattern as libaom. |
| `svt-av1-sys` | MIT | no | OPT-IN | Higher-level `svt-av1-rs` wrappers unverified. |
| x264 (C) | **GPL-2.0-or-later** | no | **DENY** | Commercial licence exists but is not compatible with our model. |
| `x264-sys` | MIT wrapper over GPL lib | no | **DENY** | Wrapper licence does not relicense the library; static linking pulls GPL into the binary. |
| x265 (C++) | **GPL-2.0-or-later** | no | **DENY** | |
| `x265-sys` | **GPL-3.0-or-later** | no | **DENY** | Wrapper is GPL-3-only over a GPL-2-or-later library — a genuine mismatch, though x265's "or later" likely resolves it. Academic for us. |
| openh264 (Cisco, C++) | BSD-2-Clause (source) | no | OPT-IN | **Critical:** Cisco's patent-royalty coverage attaches only to *Cisco's own precompiled binary*. Building from source gets you the BSD licence and **no** patent cover. This is load-bearing and is why openh264 does not solve H.264 for us. |
| `openh264-sys2` | BSD-2-Clause | no | OPT-IN | |
| libvpx (C) | BSD-3-Clause | no | OPT-IN | Licence cited from secondary sources — verify the LICENSE file directly before relying on it. |
| `vpx-sys` | MIT | no | OPT-IN | |
| `env-libvpx-sys` | **MPL-2.0** | no | **DENY** | Different crate from `vpx-sys`; wrapper is MPL even though C libvpx is BSD. |
| `ffmpeg-next`, `ffmpeg-sys-next` | **WTFPL** | no | **DENY** | Also irrelevant — we are replacing FFmpeg, not binding it. Note the obligation is set by the linked FFmpeg build's configure flags, which the crate licence says nothing about. |
| `rusty_ffmpeg` | MIT | no | **DENY** (scope) | Same: binds FFmpeg. |

## 2. Pure-Rust media crates

| Item | Licence | Verdict | Note |
|---|---|---|---|
| Symphonia (all crates) | **MPL-2.0** | **DENY** | Verified uniform MPL-2.0 across the workspace. The most painful exclusion — Symphonia covers much of our audio demux/decode surface. We implement our own. |
| `mp4parse` (Mozilla) | **MPL-2.0** | **DENY** | Same. |
| `av-format`, `av-codec`, `av-data`, `av-bitstream` (rust-av) | MIT | USE | Small, useful abstractions; evaluate whether to depend or take inspiration. |
| `av-vorbis` | — | N/A | **Does not exist** as a published crate. Only an unrelated placeholder under another name. |
| `matroska` (tuffy) | MIT OR Apache-2.0 | USE | Not under the rust-av org despite appearances. |
| `image` | MIT OR Apache-2.0 | USE | |
| `png` (image-png) | MIT OR Apache-2.0 | USE | |
| `jpeg-decoder` | MIT OR Apache-2.0 | USE | In maintenance mode; superseded by zune-jpeg as image's default. |
| `zune-jpeg` | MIT OR Apache-2.0 OR Zlib | USE | Preferred JPEG decoder. |
| `gif` (image-gif) | MIT OR Apache-2.0 | USE | |
| `tiff` (image-tiff) | **MIT only** | USE | Note the asymmetry vs its image-rs siblings — fine, but don't assume dual. |
| `image-webp` | MIT OR Apache-2.0 | USE | Pure Rust WebP. Do not confuse with `webp`. |
| `webp` (jaredforth), `webp-sys` | MIT wrapper over BSD-3 libwebp | OPT-IN | FFI; bundles ~56k lines of vendored C. |
| `ravif`, `avif-serialize` | BSD-3-Clause | USE | Pure Rust AVIF encode. |
| `zune-image` | MIT OR Apache-2.0 OR Zlib | USE | |
| `jxl-oxide` | MIT OR Apache-2.0 | USE | Pure Rust JPEG XL decoder. |
| `rgb` | MIT | USE | |
| `yuv` (yuvutils-rs) | BSD-3-Clause OR Apache-2.0 | USE | Relevant prior art for our colour conversion. |
| `dcv-color-primitives` (AWS) | **MIT-0** | USE | No attribution required. |
| `claxon` | Apache-2.0 | USE | Pure Rust FLAC decoder. |
| `flac-bound`, `libflac-sys` | MIT / BSD-3 over BSD-3 libFLAC | OPT-IN | libFLAC the *library* is BSD-3-Clause; only the FLAC command-line tools are GPL. |
| `lewton` | MIT OR Apache-2.0 | USE | Pure Rust Vorbis decoder. |
| `vorbis_rs` | BSD-3-Clause | OPT-IN | Wraps C libvorbis. |
| `audiopus` / `audiopus_sys` | ISC | OPT-IN | FFI to libopus. |
| `opus` / `magnum-opus` | MIT/Apache-2.0 | OPT-IN | FFI to libopus. |
| libopus (C) | BSD-3-Clause + **royalty-free patent grant** (Xiph, Microsoft, Broadcom) | OPT-IN | The patent grant is why Opus is GREEN for us. |
| `opus-decoder`, `opus_rs` | (check) | REVIEW | Young pure-Rust RFC 6716/8251 ports, no unsafe/FFI claimed, one passing RFC 8251 vectors. Promising as reference points but verify maturity and licence stability. |
| `minimp3` crate | MIT wrapper | OPT-IN | FFI. |
| minimp3 (C) | **CC0-1.0** | USE (as reference) | Public domain — the only major codec implementation we may read freely without clean-room concerns. Worth noting for MP3. |
| `puremp3` | MIT OR CC0-1.0 | USE | Pure Rust MP3. |
| `alac` (ebarnard) | MIT/Apache-2.0 | USE | Pure Rust ALAC decoder, independent reimplementation (not an Apple port). |
| `speexdsp-sys` | MIT | OPT-IN | |
| `speex-sys` | **MPL-2.0 AND BSD-3-Clause** (conjunctive) | **DENY** | Unusual conjunctive expression; MPL arm alone disqualifies it. |

## 3. Compression

| Item | Licence | Pure Rust? | Verdict |
|---|---|---|---|
| `flate2` | MIT OR Apache-2.0 | yes (with miniz_oxide backend) | USE — pair with miniz_oxide, not libz-sys |
| `miniz_oxide` | MIT OR Zlib OR Apache-2.0 | yes | USE |
| `zlib-rs` / `libz-rs-sys` | Zlib | yes | USE — memory-safe zlib reimplementation, a good fit for our no-unsafe posture |
| `libz-sys`, `libz-ng-sys` | MIT OR Apache-2.0 | no | OPT-IN |
| `bzip2` (trifecta) | MIT OR Apache-2.0 | FFI default, pure path exists | OPT-IN / REVIEW |
| `bzip2-rs` (paolobarbolini) | MIT/Apache-2.0 | yes, decode-only | USE |
| libbz2 (C) | `bzip2-1.0.6` (BSD-style, 4-clause-like) | no | OPT-IN |
| `lzma-rs` | MIT | yes | USE |
| `xz2`, `liblzma-sys` | MIT/Apache-2.0 | no | OPT-IN |
| liblzma / XZ Utils | **0BSD** since 5.5.2beta (was public-domain dedication); some build scripts remain LGPL/GPL | no | OPT-IN — note: the 2024 CVE-2024-3094 backdoor was a supply-chain incident, entirely unrelated to the licence change; do not conflate |
| `zstd` crate | MIT | no (FFI) | OPT-IN |
| `zstd-sys` | MIT/Apache-2.0 | no | OPT-IN |
| zstd (C, Meta) | **BSD-3-Clause OR GPL-2.0-or-later** (user's choice; separate LICENSE and COPYING files) | no | OPT-IN under the BSD arm |
| `brotli` (dropbox) | **BSD-3-Clause AND MIT** (conjunctive — both apply to different files) | yes | REVIEW then USE |
| `brotli-decompressor` | BSD-3/MIT | yes | USE |
| `brotli-sys` | MIT/Apache-2.0 over MIT C brotli | no | OPT-IN — appears stale vs the pure-Rust crates |

## 4. Crypto and TLS

| Item | Licence | Verdict | Note |
|---|---|---|---|
| `rustls` | Apache-2.0 OR ISC OR MIT | USE | Default TLS. Pure Rust. |
| `ring` | crates.io says `Apache-2.0 AND ISC`; reality is a per-file composite (ISC for new code, BoringSSL-inherited Apache-2.0/OpenSSL-style headers, Apache-2.0-OR-MIT vendored once_cell) | REVIEW | Crate-level SPDX is a summary only. cargo-deny needs an explicit `clarify` entry. Also contains unsafe — conflicts with D2 if it ends up in the default graph. |
| `aws-lc-rs` | Apache-2.0 OR ISC (crate); `aws-lc-sys` is `ISC AND (Apache-2.0 OR ISC) AND OpenSSL` | REVIEW | BoringSSL fork; same per-file caveat as ring. |
| `openssl` crate | Apache-2.0 | **DENY** (FFI) | Note the odd asymmetry: `openssl-sys` is MIT while `openssl` is Apache-2.0. |
| OpenSSL (C) | Apache-2.0 from 3.0.0 (Sept 2021); dual OpenSSL+SSLeay before that | OPT-IN | Record which major version is linked. |
| `native-tls` | MIT OR Apache-2.0 | OPT-IN | Uses platform TLS on macOS/Windows, OpenSSL on Linux. |

For our purposes crypto needs are modest — AES-CTR/CBC for CENC and HLS AES-128, plus hashes.
Prefer the RustCrypto family (`aes`, `sha2`, `md-5`, `cbc`, `ctr`) which is MIT OR Apache-2.0 and pure Rust.

## 5. Networking

| Item | Licence | Verdict | Note |
|---|---|---|---|
| `hyper` | MIT | USE | |
| `reqwest` | MIT OR Apache-2.0 | USE | TLS backend pluggable; pin rustls. |
| `quinn` | MIT OR Apache-2.0 | USE | Pure Rust QUIC. |
| libsrt (Haivision) + `srt-rs`/`libsrt-sys` | **MPL-2.0** | **DENY** | File-level copyleft. SRT support must be implemented natively or dropped. |
| librist + `librist-sys` | BSD-2-Clause | OPT-IN / REVIEW | A packaging note suggests a possible 0BSD relicense — verify upstream COPYING. VideoLAN deliberately made this one permissive. |

## 6. Playback (ffplay equivalent)

| Item | Licence | Verdict | Note |
|---|---|---|---|
| SDL2 (C) | SDL/Zlib-style permissive (since 2.0; SDL 1.2 was LGPL) | OPT-IN | |
| `sdl2` crate | MIT | OPT-IN | FFI. |
| `winit` | **Apache-2.0** (not dual) | USE | |
| `wgpu` | MIT OR Apache-2.0 | USE | |
| `cpal` | **Apache-2.0** (not dual) | USE | Thin FFI to OS audio APIs via system libs — acceptable. |
| `rodio` | MIT OR Apache-2.0 | USE | |
| `pixels` | **MIT** (not dual) | USE | |
| `softbuffer` | MIT OR Apache-2.0 | USE | |

**Recommendation for `vaco-play`:** winit + wgpu (or softbuffer for the simple path) + cpal.
All permissive, all pure Rust at the crate level, no SDL dependency. This is a strict improvement over
ffplay's SDL2 coupling.

## 7. Text, fonts and subtitles

| Item | Licence | Verdict | Note |
|---|---|---|---|
| libass (C) | ISC | OPT-IN | Verified from upstream COPYING. |
| `libass` crate (tadeokondrak) | ISC | OPT-IN | The real libass binding. |
| `ass-rs` on crates.io | MIT | N/A | **Unrelated** — it is "Aptoma Smooth Storage", nothing to do with subtitles. Naming collision. |
| `ass-core` (wiedymi/ass-rs) | MIT | REVIEW | Self-describes as 100% safe Rust, zero unsafe, a clean reimplementation. Fits D2 perfectly *if* the claim holds — verify independently that no libass-derived logic is present before adopting. |
| fontconfig (C) | HPND-style ("MIT-ish", Keith Packard) | OPT-IN / REVIEW | Primary COPYING could not be fetched; text corroborated only via mirrors. |
| freetype (C) | **FTL OR GPL-2.0-only** | OPT-IN | Under FTL, binary distribution requires a documented disclaimer that the software is based in part on the work of the FreeType Team. A real, ongoing obligation. |
| harfbuzz (C++) | "Old MIT" | OPT-IN / REVIEW | No distinct SPDX id; tools classify it as MIT but the wording differs (no explicit sublicense grant). |
| `rustybuzz` | MIT | REVIEW then USE | A manual *port* of HarfBuzz shaping, not a wrapper. Because it rewrites rather than copies, plain MIT is plausible — but if any HarfBuzz source was translated near-verbatim, Old-MIT attribution may travel with it. Flag for counsel. |
| `fontdue` | MIT OR Apache-2.0 OR Zlib | USE | Zlib needs separate allowlisting. |
| `cosmic-text` | MIT OR Apache-2.0 | USE | Strong candidate for our drawtext/subtitle text stack. |
| `swash` | Apache-2.0 OR MIT | USE | |
| `ttf-parser` | MIT OR Apache-2.0 | USE | |

**Recommendation for text rendering:** cosmic-text (+ swash / ttf-parser / fontdue underneath) gives a
fully permissive, pure-Rust shaping and rasterization stack with no FreeType attribution obligation and no
fontconfig dependency. This replaces FFmpeg's libfreetype + libharfbuzz + libfontconfig + libfribidi cluster.

## 8. AAC — a specific trap

Fraunhofer **FDK-AAC** (SPDX `FDK-AAC`, bound via `fdk-aac`/`fdk-aac-sys`) states explicitly that
**no express or implied patent licences are granted**. Debian and Fedora both classify it as non-free.
FFmpeg gates it behind `--enable-nonfree`. It must never appear in a Vaco build we distribute, and it is
not a route to AAC support. Our AAC path is a from-spec implementation (ISO/IEC 14496-3) behind a
patent-encumbered feature flag, per decision D4.

---

## 9. Open verification tasks

These were flagged during research and must be resolved before the corresponding dependency is adopted:

1. `libdav1d-sys` — confirm the crates.io repo attribution is not stale/misattributed.
2. libvpx — read the upstream LICENSE file directly rather than relying on secondary summaries.
3. librist — confirm BSD-2-Clause vs a possible 0BSD relicense against upstream COPYING.
4. `ring` / `aws-lc-rs` — per-file licence walk before any static link; write the cargo-deny `clarify` entries.
5. `brotli` — confirm the conjunctive `BSD-3-Clause AND MIT` file split.
6. `ass-core` — independently verify the "no libass-derived logic" claim.
7. `rustybuzz` — confirm the port is a rewrite, not a near-verbatim translation carrying Old-MIT obligations.
8. fontconfig — fetch the current primary COPYING if we ever depend on it.
9. `opus-decoder` / `opus_rs` — assess maturity, test coverage and licence stability.

## 10. cargo-deny starting configuration

```toml
[licenses]
confidence-threshold = 0.95
allow = [
    "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause", "BSD-3-Clause", "BSD-3-Clause-Clear",
    "ISC", "Zlib", "0BSD", "CC0-1.0", "Unicode-3.0",
]
# Everything not listed is denied, including MPL-2.0, WTFPL and all GPL/LGPL variants.

[[licenses.clarify]]
name = "ring"
expression = "MIT AND ISC AND OpenSSL"
# license-files hash to be filled in at adoption time; see open task 4.

[bans]
# Deny FFI crates from the default feature set; they are permitted only under opt-in features.
```

`cargo-about` generates `THIRD_PARTY.md` per release. A `reuse`/SPDX lint enforces per-file
`SPDX-License-Identifier: MIT OR Apache-2.0` headers across our own sources.

## 11. Conclusions that shape the architecture

1. **MPL-2.0 being denied is the single most consequential dependency decision.** It excludes Symphonia and
   mp4parse — the two largest existing pure-Rust bodies of container/audio work. We build our own. This is a
   real cost but it is also the reason the project has a reason to exist.
2. **We have permissive pure-Rust precedent for the hard parts**: rav1e (AV1 encode), jxl-oxide (JPEG XL),
   claxon (FLAC), lewton (Vorbis), puremp3 (MP3), alac, zune-jpeg, image-webp, ravif. None of these are
   dependencies we must take, but they prove the shape is achievable under a permissive licence.
3. **Text rendering is a clean win**: the cosmic-text stack removes four C dependencies FFmpeg carries.
4. **Playback is a clean win**: winit/wgpu/cpal removes the SDL2 dependency.
5. **openh264 does not solve H.264**, and **FDK-AAC does not solve AAC**. Both are patent traps dressed as
   permissive licences. Decision D4 already routes around them.
