# Vaco — Locked Project Decisions

Authoritative constraint record. All plans MUST conform. Written 2026-08-21.

## D1 — API surface: Rust-native only
No C ABI, no `libav*.so` drop-in compatibility, no mirroring of FFmpeg C struct layouts.
Deliverables are idiomatic Rust crates plus three binaries: `vaco` (ffmpeg-equivalent),
`vaco-probe` (ffprobe-equivalent), `vaco-play` (ffplay-equivalent).
CLI-level compatibility with ffmpeg/ffprobe IS a goal; ABI-level compatibility is NOT.
A C shim is explicitly out of scope for v1 and must not constrain core design.

## D2 — Safety: `#![forbid(unsafe_code)]` by default
- Every crate forbids unsafe unless it appears on the allowlist below.
- Performance comes from `std::simd` (portable, safe), careful data layout, and
  writing code that autovectorizes — NOT from inline asm or `core::arch` intrinsics.
- Allowlist (the only crates permitted `unsafe`, each requiring justification in
  its crate-level docs and a CI-enforced exception entry):
  - `vaco-hwaccel-*` — unavoidable FFI to VideoToolbox/VAAPI/D3D/Vulkan/NVDEC.
  - `vaco-io-mmap` — memory-mapped file input, if we adopt it.
  - `vaco-play-backend-*` — audio/video output device FFI.
  - Optional `-sys` wrapper crates behind non-default features.
- Everything else — all demuxers, muxers, decoders, encoders, filters, scaling,
  resampling, DSP — is 100% safe Rust. This is a hard rule, not an aspiration.
- Consequence to accept up front: a small number of kernels (CABAC/bitstream
  arithmetic decode, irregular shuffles, byte-level transposes) will be harder to
  match against hand-written asm. Plan for measurement, not assumption; if a
  specific kernel provably cannot reach parity safely, escalate it as a decision
  rather than silently reaching for unsafe.

## D3 — Licensing: MIT OR Apache-2.0 (dual)
- Our code is dual-licensed MIT OR Apache-2.0 (Rust ecosystem norm; Apache-2.0
  supplies an explicit contributor patent grant that MIT lacks).
- Dependency policy enforced in CI via `cargo-deny`:
  - ALLOW: MIT, MIT-0, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause,
    BSD-3-Clause, BSD-3-Clause-Clear, ISC, Zlib, 0BSD, Unicode-3.0, CC0-1.0.
  - DENY (hard, in the default build): GPL-*, LGPL-*, AGPL-*, MPL-2.0, CDDL,
    SSPL, EPL, proprietary/SDK-only, WTFPL (unclear provenance), and anything
    unlicensed or with an unresolved SPDX expression.
  - MPL-2.0 is denied by default despite being file-level copyleft — this rules out
    Symphonia, mp4parse, and the SRT bindings as dependencies. We implement our own.
  - `AND`-joined and per-file-composite licences (ring, aws-lc-rs, brotli, speex-sys)
    require a manual review entry before adoption, never a bare allowlist pass.
- `cargo-about` generates a THIRD-PARTY notices file per release build.
- GPL-encumbered functionality may exist as separate opt-in crates that we never
  ship in our binaries and never place in the default feature set.

## D4 — Patent posture: royalty-free default, opt-in for the rest
Rewriting in Rust changes patent exposure by exactly zero. Therefore:
- DEFAULT DISTRIBUTABLE BUILD: containers/protocols (unencumbered), AV1, VP8, VP9,
  Opus, Vorbis, FLAC, ALAC, PCM/ADPCM, lossless and utility codecs, all filters that
  are not GPL-derived, and decode-only support for codecs whose essential patents
  have lapsed.
- OPT-IN, BUILD-IT-YOURSELF (in-tree, behind non-default Cargo features, never in
  our published binaries): encoders for HEVC, VVC, AAC, AC-3/E-AC-3, DTS, and any
  other AMBER/RED item the legal register identifies.
- Feature-flag naming must make the posture obvious, e.g. `patent-encumbered-hevc-encode`.
- CI publishes the default binary with the encumbered feature set provably absent
  (assert on the compiled feature list, not on intent).

## D5 — v0.1 milestone: ffprobe on modern containers
Demux MP4/MOV, Matroska/WebM, MPEG-TS; parse H.264/HEVC/AV1/AAC/Opus stream
headers (parse only — no decode); emit the complete ffprobe writer surface
(default/compact/csv/flat/ini/json/xml) with byte-identical output against the
reference tool for the covered sections. Zero encoders, zero filters.

## D6 — Correctness strategy: differential testing + fuzzing are first-class
Not an afterthought; part of the v0.1 foundation.
- DIFFERENTIAL HARNESS: for a given input file and a given argument vector, run
  both the reference `ffmpeg`/`ffprobe` binary and ours, and require identical
  output. Compare at several levels: exact bytes (ffprobe text/JSON/XML output,
  remuxed container bytes where deterministic), per-frame checksums (framecrc /
  framemd5 equivalents), decoded pixel/sample data exact-match, and structured
  metadata diff with an explicit, reviewed allowlist of permitted divergences.
- The reference binary is used ONLY as a black box producing observed outputs.
  Its source is never consulted by implementers. This is what keeps the
  differential harness compatible with the clean-room policy (D7) — recording
  observed behaviour of a shipped binary is not copying expression.
- FUZZING: `cargo-fuzz`/libFuzzer plus `arbitrary` for structured input generation.
  Every demuxer, every bitstream parser, every decoder gets a fuzz target from the
  day it lands — a component without a fuzz target is not "done".
  Because everything is safe Rust, fuzzing targets panics, unbounded allocation,
  non-termination/hangs, and arithmetic overflow in debug — not memory corruption.
  Add a differential fuzzer: mutate real media, feed to both implementations,
  assert agreement or that both reject.
- Corpus is seeded from FATE sample media and public conformance suites; corpora
  are cached in CI and minimized.
- Both harnesses gate merges.

## D7 — Clean-room policy
- FFmpeg source may be read ONLY by designated "spec writers", whose output is
  behavioural specification prose in `planning/` and `docs/` — never code.
- Implementers work from public specifications (ITU-T / ISO-IEC / IETF RFC / SMPTE /
  AOM), from our own specification documents, and from black-box observation of the
  reference binaries. They do not open FFmpeg's source.
- Never reproduced from FFmpeg: algorithm implementations, constant tables that
  represent authorial choice, comments, code structure, test reference data.
- Freely used: format/codec/option/CLI NAMES and documented semantics (interface
  facts), spec-dictated constants derived independently from the spec text.
- Evidence trail: every implementation PR carries a provenance trailer naming the
  specification and section it was written from.
- See `planning/research/07-legal-patents-licensing.md` for the full analysis and
  the questions that need real counsel.

## D8 — Toolchain
- Pinned nightly (`rust-toolchain.toml`), required for `std::simd` (`portable_simd`).
- Cranelift backend for debug/dev builds; LLVM for release.
- Release pipeline: LTO (fat), `codegen-units=1`, PGO, and BOLT evaluated per target.
- `just` at the repo root is the single entry point for every developer command.
- Criterion (or divan) benchmarks with CI regression tracking on every DSP kernel
  and every end-to-end pipeline.

## D9 — Amendments from the legal register (2026-08-21)

Added after `planning/research/07-legal-patents-licensing.md` completed. These override anything
above that conflicts.

- **AV1 is AMBER, not GREEN.** Dolby — which is not an AOMedia member and therefore carries no
  royalty-free obligation — sued Snap on 2026-03-23 (D. Del. 1:26-cv-00317) over AV1 and HEVC,
  seeking an injunction. We still ship AV1; it remains the best available option. But the
  "royalty-free codec" story is materially weaker than it was, and D4's default build rests on it.
  Track this case.
- **HEVC and VVC are RED and cannot be mitigated.** Access Advance acquired Via LA's HEVC/VVC pools
  on 2025-12-15 and consolidation is incomplete. With multiple pools, paying one does not stop the
  other suing. No structure fixes this.
- **AAC is RED, and is our most painful exclusion.** Mitigation that survives: the Via LA pool
  charges per encoder/decoder unit, not per bitstream — so **AAC remuxing stays in the default
  build**. Only encode and decode are gated.
- **E-AC-3 is unresolved.** Its last-patent-expiry (2026-01-30) rests on a single hedged secondary
  source. Load-bearing and unverified — do not ship E-AC-3 until confirmed by counsel.
- **GREEN and shippable:** MPEG-1/2, MPEG-4 Part 2, H.263, MP3, AC-3, Opus, Vorbis, FLAC, ALAC,
  JPEG, JPEG XL, VP8, G.711/722/729, CineForm.
- **Hardware and system-codec delegation is our best patent mitigation.** Users get H.264/HEVC
  through silicon they already hold a licence for, while our binary contains no software codec for
  either. This raises the priority of the `vaco-hw-*` crates from "nice to have" to "strategic".
- **Never publish a "full" convenience binary.** This is the single mistake that undoes every other
  precaution. Where a GPL-licensed external encoder is genuinely wanted, prefer `exec`-ing the
  user's own installed binary over linking it — the process boundary solves the GPL problem and the
  patent problem at once.
- **cargo-deny alone is not sufficient.** crates.io metadata lies: the `x264` and `x265` crates
  declare MIT while statically linking GPL libraries, and `freetype-sys` declares MIT over FTL/GPL-2.
  A licence check that trusts declared metadata will happily pass a GPL binary. CI therefore needs a
  **separate manual-review audit job for every `*-sys` crate**, keyed on what the crate actually
  links rather than what it declares.
- **Clean-room model is tiered, not blanket.** Spec-first implementation plus a per-module
  contamination rule covers roughly 90% of the work. The formal two-team dirty/clean protocol is
  reserved for spec-less formats (Bink, RealVideo, Indeo, the game codecs) where no public
  specification exists to work from.
- **Interface names are implementable; text is not.** CLI option names and ffprobe JSON/XML field
  names may be reproduced (Lotus v. Borland; Google v. Oracle fair use; SAS v. WPL). Help strings,
  comments, option-table prose and the `.xsd` file itself may not.
- **Constant tables come from the specification, never from FFmpeg.** Spec-dictated tables are
  merger / scenes a faire. FFmpeg's *reordered or pre-scaled* variants of those tables are its own
  expression and are off limits.
- **Apache-2.0's patent grant binds our contributors only.** It does nothing against Dolby or the
  pools. State this plainly rather than letting it read as protection it is not.

Five questions genuinely need counsel: contributory infringement exposure for AMBER codecs; the
jurisdiction of the entity that ships binaries (decide before incorporating); a commissioned
freedom-to-operate search; a written clean-room opinion; and trademark clearance for the `vaco` name
plus any opt-in `ffmpeg`-named compatibility shim.

## D10 — Dependency policy: pure Rust, trusted, maintained (2026-08-21, revised)

**The rule.** External crates are welcome, including for codecs, containers and signal processing —
subject to three hard gates and one judgement call.

### Gate 1 — Pure Rust. No exceptions.
Zero `-sys` crates, zero FFI, zero vendored or bundled C/C++, zero build scripts that compile native
code or probe for system libraries. `cargo tree` must contain nothing that links a foreign library.
This rules out every binding to dav1d, libaom, SVT-AV1, libvpx, x264, x265, openh264, libopus,
libvorbis, libFLAC, libass, FreeType, HarfBuzz, fontconfig, SDL2, libsrt and OpenSSL — regardless of
how good or how permissively licensed they are.

Rationale: FFI is the hole through which every guarantee this project makes leaks out. Memory safety
stops at the boundary, the licence of the linked library is not what the wrapper crate declares (the
`x264` and `x265` crates declare MIT while statically linking GPL — see D9), cross-compilation breaks,
and the build stops being reproducible. One rule at the boundary removes all of it at once.

### Gate 2 — Licence, per D3.
MIT / MIT-0 / Apache-2.0 / BSD-2 / BSD-3 / BSD-3-Clear / ISC / Zlib / 0BSD / CC0 / Unicode-3.0.
MPL-2.0 is denied, which still excludes Symphonia and `mp4parse` — on licence grounds, not purity.

### Gate 3 — Trusted and maintained.
Assessed at adoption and re-checked each release. A crate must clear all of:
- **Alive**: a release or substantive commit within ~12 months; issues and PRs being triaged.
- **Adopted**: meaningful download counts and reverse-dependencies, or a named maintainer with a
  track record. Not a one-person crate at 0.1.0 with 300 downloads.
- **Sound**: no open RUSTSEC advisory; `cargo-audit` clean.
- **Shallow**: a dependency tree we are willing to own transitively. A crate that drags in forty
  transitive dependencies is a liability whatever its own quality.
- **Unsafe-light**: measured with `cargo-geiger`. See the note below — this is where the real
  tension sits.
- **Vendorable**: we could fork and maintain it ourselves if it were abandoned. If forking is
  implausible, adopting is too.

Every adoption is recorded in `docs/dependencies.md` with the assessment, the date, and who signed
off. Adding a dependency is a reviewed decision, not a `cargo add`.

### The judgement call — build or buy
Prefer a crate that clears all three gates over writing our own. Write our own when:
- No crate clears the gates for that job.
- The crate's model does not fit ours — e.g. it cannot produce our `Frame` type without a copy, or it
  owns I/O when we need to drive it, or it cannot be driven incrementally by our scheduler.
- We need a capability it does not provide. The concrete example: **bit-exact i32 fixed-point
  transforms**, which several audio codecs require for conformance and which general-purpose FFT
  crates (`rustfft`, `realfft`) do not offer. That alone is likely to make `vaco-tx` ours.
- It sits on the hot path and we need control over its SIMD and allocation behaviour.

Where a crate is a good fit for part of a job but not all of it, depending on it for the part it does
well is correct — a codec crate does not have to be all-or-nothing.

### The unsafe tension — stated plainly, needs your call at adoption time
D2 puts `#![forbid(unsafe_code)]` on our crates. That lint is per-crate; it does not and cannot reach
dependencies. Several strong pure-Rust media crates use `unsafe` internally, sometimes heavily
(`rustfft` and `rav1e` both do, for SIMD). So "no unsafe" is a guarantee about **our code**, not
about the process.

The policy: measure it with `cargo-geiger`, prefer the lower-unsafe option when two crates are
otherwise comparable, and treat a heavy-unsafe crate on a hot path as a deliberate trade-off to be
argued in the adoption record — not an automatic disqualification, and not something to wave through
silently either. Where we want the guarantee to be genuinely end-to-end, that is an argument for
writing that particular component ourselves.

### What this means in practice
Back on the table, subject to the gates: `image` and its family, `png`, `zune-jpeg`, `gif`, `tiff`,
`image-webp`, `jxl-oxide`, `ravif`, `rav1e`, `claxon`, `lewton`, `puremp3`, `alac`, `matroska`,
`zlib-rs`/`miniz_oxide`, `lzma-rs`, `brotli`, the RustCrypto family, `rustls`, `hyper`, `quinn`,
`cosmic-text`/`swash`/`ttf-parser`/`fontdue`, `wgpu`/`winit`/`cpal`, `rayon`, and the usual utility
crates.

Still out: everything FFI (Gate 1), everything MPL (Gate 2), and anything failing Gate 3.

Note that the surviving set is genuinely capable — `rav1e` alone shows a permissive pure-Rust AV1
encoder is achievable — but it is nowhere near complete. There is no pure-Rust H.264, HEVC, VP9,
AAC, AC-3 or Opus encoder of production quality, no pure-Rust MP4/Matroska/MPEG-TS muxer at FFmpeg's
level, and no pure-Rust swscale equivalent. The large majority of this project is still ours to
write; the crates reduce the periphery, not the core.

## D11 — External codecs sit behind our own API boundary (2026-08-21)

Refines D10. The gates in D10 decide *whether* a crate may be used; this decides *how*.

### The problem
Bit-identical output against the reference `ffmpeg` is a project requirement (D6). An external crate
was written to satisfy its own correctness criteria, not ours — it may round differently, handle
edge cases differently, or simply produce different-but-valid output. We cannot control that from
outside, and we will not know how bad it is until we measure. So any external codec is provisional
by nature: useful to move fast now, possibly wrong for us later.

### The rule
**Every external codec, container or signal-processing crate is reachable from exactly one Vaco
crate, and that crate exposes only our own API.**

- The wrapping crate is named like any other: `vaco-codec-flac`, `vaco-demux-matroska`. Callers cannot
  tell from the name, the API, or the types whether it wraps or implements. That is the point.
- It implements our traits (`Decoder`, `Encoder`, `Demuxer`, `Muxer`) over our types (`Frame`,
  `Packet`, `CodecParameters`). No external type appears in its public API — not in a signature, not
  in an error variant, not in a re-export, not in a trait bound.
- No other crate in the workspace may list that external crate as a dependency. **CI enforces this**:
  a check asserts every third-party media crate appears in exactly one `Cargo.toml` under
  `crates/`. A second occurrence fails the build.
- Errors are translated at the boundary into our error taxonomy. Panics from a dependency are a
  defect we track, not something callers see.

Swapping a backend then means rewriting one crate's internals. Nothing outside it changes, and the
existing tests for that crate are exactly the acceptance criteria for the replacement.

### Backend selection as a feature
Where both a wrapped and a native implementation exist — during a migration, or permanently — the
crate carries mutually exclusive backend features:

```toml
[features]
default        = ["backend-external"]
backend-external = ["dep:claxon"]
backend-native   = []          # our own implementation
```

Both backends satisfy the same tests. This makes replacement incremental and reversible, and it lets
the differential harness compare three ways at once: our implementation against the wrapped one,
and both against the reference binary. That three-way comparison is how we find out whether a
divergence is our bug or a genuine difference in interpretation.

### Per-codec fidelity is measured, recorded, and gates promotion
Every wrapped codec carries a fidelity grade in `docs/codec-status.md`, established by the
differential harness and re-checked in CI:

| Grade | Meaning | Consequence |
|---|---|---|
| **Exact** | Byte-identical to the reference for the whole test corpus. | Ship it. Revisit only for performance. |
| **Equivalent** | Not byte-identical, but within a documented, justified tolerance (last-bit float, a permitted metadata difference). | Ship it, with the tolerance recorded and reviewed. |
| **Divergent** | Differs in ways we cannot justify. | Blocks the codec from the default build; schedules a native implementation. |
| **Unmeasured** | Not yet run against the corpus. | Cannot ship in the default build. |

A crate is not adopted on reputation. It is adopted, measured, and then either kept or replaced —
and the grade makes that decision evidence-based rather than a matter of opinion.

### Why this is the right trade
It separates two decisions that would otherwise be welded together: *what our architecture looks
like* and *who wrote the codec today*. The first is expensive to change and we get it right now; the
second is cheap to change and we defer it until we have measurements. The cost is one thin
translation layer per external crate, which we would want anyway to keep foreign types out of our
core.

## D12 — SIMD substrate: `fearless_simd`, not `std::simd` (2026-08-21)

Resolves the conflict between D2 (`forbid(unsafe_code)`) and the performance goal. Supersedes the
`std::simd` assumption in D8, in `10-architecture.md` §7.3, and in plans 11 and 12.

### The problem it solves
`#[target_feature]` functions are unsafe to call, because the caller cannot prove to the compiler that
the CPU has the feature. So runtime ISA dispatch — one binary that detects AVX2/AVX-512/NEON at
startup and uses it, which is exactly what FFmpeg does — is unreachable under `forbid(unsafe_code)`.
`std::simd` alone only gives us whatever the *build target* permits, so a normally-distributed binary
would sit at baseline x86-64 and leave most of the hardware unused.

`fearless_simd` (Linebender) closes this with a capability-token design: zero-sized marker types
*witness* that a given SIMD level is available. Each function is monomorphised per level, dispatch
picks the best at runtime, and because the token is proof, the intrinsic calls are safe at every call
site. The unsafe is encapsulated once inside the crate — described by its authors as "orders of
magnitude less than alternatives" — rather than spread across our kernels.

### Gate assessment (per D10)
| Gate | Result |
|---|---|
| **1 — pure Rust, zero FFI** | **Pass.** Zero dependencies at all; no `-sys`, no build-script native compilation. |
| **2 — licence** | **Pass.** Apache-2.0 OR MIT — an exact match for our own dual licence. |
| **3 — trusted & maintained** | **Pass.** Linebender (Raph Levien). v0.7.0 released 11–12 Aug 2026; v1.0 targeted early September 2026; API described as stable for nearly a year with no breaking changes planned for 1.0. 417 stars, 311 commits, active development in the open. Zero dependencies means the shallowest possible tree. Small enough to fork and maintain if it were ever abandoned. |

MSRV is Rust 1.89.

### What it provides
Levels: `Fallback` (scalar), `Sse2`, `Sse4_2`, `Avx2`, `Avx512` on x86/x86-64, plus a `Level` enum for
runtime detection. Documented target platforms include `aarch64-apple-darwin` and
`wasm32-unknown-unknown`.

Operations confirmed present: add/sub/mul/div/neg/abs; `mul_add`/`mul_sub` (FMA); full bitwise set;
`shl`/`shr` plus variable shifts; comparisons; `select`; `zip_low`/`zip_high`, `unzip_low`/`unzip_high`,
`interleave`/`deinterleave`; `slide`; `combine`/`split`; `swizzle_dyn` and `swizzle_dyn_precise`;
`narrow`/`widen` and **`saturating_narrow`** (the `packuswb` we need); float math; and mask operations
including `to_bitmask`/`from_bitmask` and `any_true`/`all_true`. Integer types u8 through i64, floats
f32/f64.

### Two open risks — verify before committing kernel code
1. **Operation gaps.** Not evident in the API: **saturating add/sub**, **horizontal reductions**
   (sum/min/max across lanes), **widening multiply-add** (the `pmaddubsw`/`pmaddwd` shape that plan 12
   names as its single largest performance risk), **average** (`pavgb`, heavily used in motion
   compensation), and **absolute difference / SAD** (`psadbw`, the core of motion estimation).
   Several can be composed from what exists — saturating add via widen/add/`saturating_narrow`, SAD via
   sub/abs/widen/add — but composition costs instructions on exactly the hottest paths. **Action:**
   benchmark the composed forms against the plan-12 targets early, and raise the gaps upstream; v1.0
   is two weeks out and the project is actively taking feedback, so this is a good moment to ask.
2. **aarch64 NEON is implied but unconfirmed.** `aarch64-apple-darwin` is a documented target and the
   v0.7 notes reference aarch64 build measurements, but no explicit `Neon` level appeared in the
   documentation I could read. This is critical — Apple Silicon and ARM servers are primary targets.
   **Action:** confirm before any kernel work begins.

### Consequences
1. **The kernel authoring model changes.** Plans 11 and 12 were written against `std::simd` with
   const-generic lane counts. `fearless_simd` uses fixed-width vectors and level-generic functions
   instead. Both plans need revision; the worked example in plan 12 §4 must be rewritten against the
   real API.
2. **`vaco-simd` becomes an adapter, exactly as D11 prescribes.** `fearless_simd` is reachable from
   `vaco-simd` and nowhere else; our kernels are written against our own `KernelSet` abstraction. If
   the operation gaps prove fatal we swap the substrate by rewriting one crate.
3. **We can probably drop the nightly pin.** `std::simd` was the only mandatory reason for nightly
   (D8). `fearless_simd` builds on stable 1.89+. Cranelift dev builds still want nightly, but that is
   a convenience rather than a requirement — so the plan becomes **stable for release builds,
   optional nightly for fast local iteration**. This is a real gain for reproducibility, for CI, and
   for contributors.
4. **`#![forbid(unsafe_code)]` survives across the entire workspace**, including every DSP kernel,
   with no exception list beyond the pre-existing `vaco-hw-*` crates. That is a stronger guarantee
   than the scoped-exception option would have given.
5. The honest caveat, per D10: the crate contains unsafe internally. The guarantee remains "no unsafe
   in our code", not "no unsafe in the process" — but here the unsafe is small, centralised, written
   by people who specialise in exactly this problem, and shared with the wider Rust graphics
   ecosystem. That is a considerably better position than us hand-rolling dispatch.

### D12 addendum — risks resolved (2026-08-21)

Both open risks were investigated against `fearless_simd` v0.7.0 source and docs. Outcome: **adoption
confirmed**, with the performance forecast revised *downward* and one new constraint discovered.

**Risk 2 — aarch64 NEON: CLOSED.** `Level::Neon(Neon)` exists under `cfg(target_arch = "aarch64")`,
with `as_neon()` and a 224 KB generated backend. Bonus: aarch64 has a single level, so there is no
multiversioning cost on Apple Silicon or ARM servers at all.

**New constraint — `dispatch!` is safe, `kernel!` is not.** `dispatch!` expands to no `unsafe`
(it routes through `Simd::vectorize`), so `#![forbid(unsafe_code)]` survives across the whole DSP
layer. But the `kernel!` macro **does** expand `unsafe` into the calling crate, so raw intrinsics are
closed to us. We use `dispatch!` exclusively. This was not known when D12 was written and it bounds
what we can reach.

**Risk 1 — operation gaps: real, mostly cheap, one expensive.** All five named gaps confirmed absent,
plus integer `abs`. Compositions and their costs:

| Missing op | Composition | Cost |
|---|---|---|
| unsigned saturating add | `min(!b) + b` | 3 ops |
| unsigned saturating sub | `max(b) - b` | 2 ops |
| rounded average (`pavgb`) | `(a\|b) - ((a^b) >> 1)` | 4 ops, exact, stays in-width |
| absolute difference | `max - min` | 3 ops |
| integer abs | `max(x, -x)` | 2 ops |
| horizontal reduction | tree | ~2·log₂N — hoist out of loops |
| **widening multiply-add** | **no composition** | **~6× for the `pmaddwd` shape; ~2.2–2.5× on the 8-tap u8 FIR** |

Widening multiply-add is the one that hurts, and it is materially worse than plan 12's original ~1.4×
estimate. It is also the shape that H.264/HEVC motion compensation and SAD/SATD are built from.

**Revised performance forecast — stated plainly rather than smoothed.** Motion compensation moves to
0.75–0.95×; SAD and SATD to 0.70–0.90× (LLVM's automatic `psadbw` reconstruction can no longer be
relied on once we go through explicit intrinsics). Colour conversion and filters move *up*, on
runtime AVX-512 reach we previously could not have. The weighted headline for 1080p H.264 decode
becomes **0.96× before PGO**, down from 0.98×.

That number is also measured against a fairer basis: the old figure compared two builds both targeting
v3, which is not how software actually ships. The new one compares *as shipped* — our
runtime-dispatching binary against a distribution ffmpeg — and that comparison is the one users
experience. PGO is expected to bring it to roughly parity or slightly above.

**Consequences accepted:**
- Toolchain moves to **pinned stable 1.89**. Cranelift dev builds become opt-in via environment
  variables in a single `just` recipe rather than a nightly pin. D8 is amended accordingly.
- The distribution baseline drops from x86-64-v3 to **v2**, since runtime dispatch makes a lower
  floor strictly better rather than a compromise.
- Plan 12's multi-artifact v2/v3/v4 build strategy and launcher are **withdrawn** — no longer needed.
- Plan 11's fork decision F5 (build-time ISA selection) is superseded by F5′, with the original
  reasoning preserved as a visible trail.

**Residual risk, unresolved and worth naming.** `fearless_simd` is a small crate from a small project,
and our entire DSP layer would rest on it. It is forkable — that is why it clears Gate 3 — but forking
is a cost we would actually pay, not a theoretical escape hatch. The D11 adapter keeps the *interface*
blast radius to one crate, but a substrate change would still mean rewriting kernel bodies across the
codebase. Mitigation: engage upstream before v1.0 ships in early September, and put the widening
multiply-add gap to them directly — the project is actively taking feedback and this is the moment.

## D13 — Hardware acceleration ships by default (2026-08-21)

Corrects `10-architecture.md`'s "Optional — hardware (the unsafe allowlist)" section, which excluded
the `vaco-hw-*` crates from the default build. **Containing `unsafe` was the stated reason. That is
not a good reason.** Unsafe is acceptable where it is the only way to do something, and talking to
video hardware is exactly that case.

### The rule
Hardware acceleration is **in the default build wherever it is legally distributable and the platform
supports it.** The test is legal distributability and correctness, never the presence of `unsafe`.

### Why this matters more than convenience
The legal register (`research/07`) identifies hardware delegation as our single best patent
mitigation. Shipping hardware decode by default means users get H.264 and HEVC through silicon whose
vendor already paid the licence, while our binary contains **no software codec for either**. Combined
with D9's recommendation never to ship a "full" convenience binary, this turns hardware acceleration
from a nice-to-have into the load-bearing answer to the codecs we cannot ship in software. It should
be sequenced accordingly — early, not late.

### Prefer safe abstractions where they actually reach — and know where they stop
Two different problems, often conflated:

1. **GPU compute** — filters, scaling, colour conversion, tone mapping. `wgpu` covers this fully, in
   safe Rust, portably. `vaco-filter-gpu` stays `#![forbid(unsafe_code)]`. Plan 16 already argues this
   well: ~87 upstream per-vendor filter variants collapse into ~16 WGSL kernels in one crate.
2. **Fixed-function video decode and encode** — the dedicated silicon blocks. **`wgpu` does not
   expose these** (gfx-rs/wgpu issue #2330 remains an open discussion, not shipped functionality), and
   no safe portable abstraction exists. These require vendor or OS APIs, and therefore `unsafe`.

Do not let (1)'s success suggest (2) is solvable the same way. It is not, today.

### Backend strategy
| Backend | Binding crate | Platforms | Note |
|---|---|---|---|
| **Vulkan Video** | `ash` (pure-Rust Vulkan bindings) | Linux, Windows, Android | **The best single investment.** Khronos finalised H.264 and H.265 decode, later encode, and `VK_KHR_video_decode_vp9`; AV1 decode also specified. One vendor-independent API covering most of the world. |
| **VideoToolbox** | `objc2-video-toolbox` (objc2 project, pure Rust, no C compilation) | macOS, iOS | Required — MoltenVK does not implement Vulkan Video, so Apple cannot be reached through the Vulkan path. |
| **D3D12 Video** | `windows` (Microsoft, pure Rust) | Windows | Optional; Vulkan Video already covers Windows. Add only if measurement justifies it. |
| VAAPI / NVDEC | — | Linux | Only if Vulkan Video proves insufficient in practice. Prefer not to. |

### Gate 1 refined
D10 Gate 1 bans FFI. That was aimed at three specific harms: build complexity and broken
cross-compilation, licence laundering through `-sys` wrappers (D9: the `x264` crate declares MIT while
linking GPL), and unsafety at the boundary. **Calling the operating system's own video API through a
pure-Rust binding crate causes none of them.**

So Gate 1 is stated precisely:

- **Banned:** vendored or compiled third-party C/C++ libraries — anything with a `links` key, a
  build script invoking `cc`/`cmake`, or bundled foreign source. This still excludes every `-sys`
  codec binding.
- **Permitted, in `vaco-hw-*` crates only:** pure-Rust binding crates that declare externs against an
  OS or driver API already present on the machine — `ash`, `objc2-*`, `windows`, `wgpu`. These compile
  no foreign code, vendor no foreign library, and launder no licence.

The distinction is between *statically linking someone's codec* and *asking the operating system to
use its own*. Only the first was ever the problem.

### Unsafe discipline
1. **Unsafe blocks are tiny and mechanical** — handle wrapping, RAII lifetime management, buffer
   handoff. Every piece of logic lives in safe Rust above them. The audit surface must stay small
   enough that a reviewer can hold all of it at once.
2. **Every `unsafe` block carries a `// SAFETY:` comment** naming the invariant and why it holds.
   `clippy::undocumented_unsafe_blocks` is denied, so this is enforced rather than encouraged.
3. **The crates remain listed** in the D2 allowlist — not to exclude them from the default build, but
   so the audit set is explicit and CI can assert nothing else has drifted into using unsafe.

### Verification — what actually works, and what does not
**Miri cannot execute foreign functions.** It will not validate a VideoToolbox or Vulkan call, and
claiming it as our safety net would be false comfort. What we actually do, in order of value:

1. **Differential testing against our own software decoder.** The strongest check available: decode
   the same bitstream through hardware and through our software path and compare. Hardware decoders
   are conformance-tested by their vendors, so a mismatch is our binding bug with high probability.
   This is a real oracle, not a proxy — and it costs us nothing extra, since the software decoder must
   exist anyway.
2. **Sanitizers.** `-Zsanitizer=address` and `=thread` in a dedicated nightly CI job. These instrument
   at the LLVM level and catch use-after-free, buffer overrun and data races across the FFI boundary,
   which is exactly the class of bug the bindings can produce. The nightly requirement is fine — it is
   a test-time tool and does not affect the stable release toolchain (D12).
3. **Miri on the pure-Rust unsafe, in isolation.** Pointer arithmetic, slice construction from raw
   parts, transmutes — with the hardware behind a mock backend. Genuinely useful for the parts Miri
   can see; useless for the calls themselves. Structure the crates so those parts are separable.
4. **Fuzz the wrapper layer against a mock backend**, driving malformed and adversarial inputs through
   the safe API to prove the unsafe below cannot be reached in a bad state.
5. **Leak and lifetime checks** under sustained decode, since the common real failure is a handle or
   surface leak rather than memory corruption.

### Consequences
- `10-architecture.md`'s hardware section is rewritten: `hw-<backend>` features are **on by default per
  platform**, not opt-in.
- `vaco-hw-vulkan-video` becomes a high-priority Wave 2/3 item rather than a late addition, because it
  is how H.264 and HEVC reach users at all.
- One thing to confirm with counsel: feeding a bitstream to a licensed hardware decoder is the
  standard pattern every media application on every platform already follows, and we ship no
  patented decoding logic ourselves. It should be clean. It is worth a sentence in the written
  opinion anyway, since the whole default-build strategy now rests on it.

## D14 — Cross-plan resolutions (2026-08-21)

Four issues raised by plan 18 that span documents and needed deciding rather than deferring.

### D14.1 — Layering correction: `vaco-codec-core` sits below `vaco-format-core`

Plan 18 is right. Demuxers need codec parameters and bitstream parsers; if `vaco-format-core` sat
below `vaco-codec-core`, every format crate would end up depending on codec crates and the acyclicity
rule would collapse.

The corrected order is `vaco-codec-core` → `vaco-format-core`, and demuxers reach parsers through an
injected `ParserProvider` trait rather than by naming a codec crate directly. **No format crate ever
depends on a codec crate.** This also keeps the D11 adapter boundary intact — a demuxer cannot
accidentally reach a wrapped external codec.

`10-architecture.md` §3 is amended accordingly.

### D14.2 — TLS: `rustls` with a pure-Rust crypto provider

Plan 18 found that **both** of rustls's production crypto providers fail D10 Gate 1: `ring` and
`aws-lc-rs` each vendor and compile C and assembly. The refined Gate 1 in D13 does not rescue them —
they are vendored third-party C, not OS APIs, which is exactly what the gate bans.

Decision: **`rustls` + `rustls-rustcrypto`**, subject to a throughput benchmark before we commit.
Rationale: we are a media tool, not a CDN — TLS sits on manifest and segment fetches, not on the
decode hot path, so a provider that is somewhat slower is very likely acceptable. Benchmark it against
representative HLS/DASH workloads; if it proves inadequate, escalate rather than quietly relaxing the
gate.

Unplanned benefit: excluding `ring` also removes the per-file licence audit D9 flagged as necessary,
since `ring`'s `LICENSE` is not a parseable single SPDX expression and needed a `cargo-deny` clarify
entry. One decision retires two problems.

### D14.3 — Gate 1 covers `std` and thin syscall wrappers explicitly

D13 drew the line between *vendored third-party C* (banned) and *OS APIs reached through pure-Rust
bindings* (permitted). That principle already covers this, but plan 18 is right that it should be
written down rather than inferred: `std` itself, and thin pure-Rust wrappers over OS syscalls such as
`socket2`, are **permitted everywhere**, not just in `vaco-hw-*`. They compile no foreign code and
vendor no foreign library.

The permitted list is therefore: `std`; thin syscall wrappers (`socket2`, `rustix`-style); and, in
`vaco-hw-*` and `vaco-filter-gpu` only, pure-Rust bindings to OS/driver media and graphics APIs
(`ash`, `objc2-*`, `windows`, `wgpu`).

### D14.4 — `-show_frames` moves from v0.1 to v0.2

Plan 18 caught a genuine contradiction between D5 and plan 14. D5 scopes v0.1 as **parse only, no
decode**. But `-show_frames` reports decoded-frame properties and therefore requires a decoder, so
plan 14's v0.1 acceptance matrix cannot include it without breaking D5.

`-show_frames` (and `-show_packets`'s frame-side counterparts, `-count_frames`, and `-analyze_frames`)
move to **v0.2**, alongside the first decoders. v0.1 remains: demux MP4/Matroska/MPEG-TS, parse
H.264/HEVC/AV1/AAC/Opus stream headers, and emit byte-identical output for `-show_format`,
`-show_streams`, `-show_packets`, `-show_programs`, `-show_chapters` and the version/pixel-format
sections across all six writers.

This makes v0.1 smaller and sharper, which is the right direction for a first milestone.

### Two findings worth carrying forward

**Build-or-buy inverts between codecs and containers.** A codec has a specification acting as a shared
oracle, so an independent implementation can converge on identical output. A container demuxer has no
such oracle for the *derived* fields ffprobe prints — bit rates, durations, frame counts, probe scores
are estimation policy, not spec text. Plan 18's conclusion: every third-party demuxer is **Divergent
by construction** under D11. `matroska` additionally fails on model fit (no packets, no lacing, no
encodings). Only infrastructure crates survive — `quick-xml`, RustCrypto, `flate2`, `id3`, `ureq`.

The corollary is the good news: **muxing is deterministic**, so containers are where D6's byte-identity
requirement is most fully achievable, and where the differential harness will earn its keep first.

**Half of FFmpeg's format list cannot be clean-roomed either.** 192 of 368 demuxers (52%) have no
public specification — mirroring plan 15's finding that ~300 of ~605 decoders are in the same
position. The two halves of the long tail are the same long tail. This is now a well-evidenced,
project-defining constraint rather than a suspicion, and it needs to shape how scope is described
publicly.

## D15 — "Cannot be clean-roomed" was wrong (2026-08-21)

Corrects plans 15 and 18, both of which classified roughly half of FFmpeg's inventory — ~300 decoders
and 192 demuxers — as impossible to reimplement cleanly. That framing is a legal error, and it was
propagated into the roadmap and into the project's headline description. This decision replaces it.

**Not legal advice.** No counsel is engaged. This is an engineering risk assessment with citations,
and it flags where the ground is solid versus where it is judgement.

### What the law actually says

**17 U.S.C. §102(b):** copyright protection does not extend to "any idea, procedure, process, system,
method of operation, concept, principle, or discovery, **regardless of the form in which it is
described, explained, illustrated, or embodied** in such work."

That trailing clause is the one that matters here. An algorithm does not become protected by being
written down, and reading a description of it does not taint the algorithm. Congress intended §102(b)
to codify the *Baker v. Selden* exclusion of procedures, processes, systems and methods of operation.

**Merger doctrine:** where an idea can be expressed in only one or a few ways, idea and expression
merge and the expression is unprotectable. Functional design elements fall outside copyright's scope
even inside a copyrighted work — they are patent's territory, not copyright's.

**Applied to a reverse-engineered codec**, this is close to the paradigm case for merger. A Huffman
table that a decoder *must* contain in order to decode the format is not an authorial choice; it is
the only possible expression of "how this format encodes symbols". The same holds for bitstream
layouts, magic numbers, field orders and state machines dictated by the format. These are facts about
an artifact, and facts are not copyrightable.

**So the conclusion in plans 15 and 18 is wrong.** Bink, Indeo, RealVideo, Smacker and the rest of the
long tail are not legally blocked. The user's instinct was correct: describe the algorithm — in
pseudocode, in prose, in a state diagram — and implement from that description.

### What *is* genuinely constrained

The narrow set, and it is much smaller than half the inventory:

1. **Trained model weights.** `nnedi`'s neural-network coefficients are not an algorithm; they are the
   output of somebody's training run over their data. There is no functional necessity fixing those
   particular numbers, and no specification from which to regenerate them. This is the clearest
   genuine block in the entire inventory, and plan 16 already identified it correctly.
2. **Hand-tuned perceptual and aesthetic tables not dictated by the format.** Psychoacoustic model
   constants, the Shibata noise-shaping curves (plan 17 already caught this and generates its own by
   fitting to ISO 226), quantiser matrices chosen by taste rather than specified. Where a table is an
   author's judgement rather than a format requirement, generate our own equivalent.
3. **Literal expression** — code, comments, naming, and the specific decomposition into functions
   where genuinely different decompositions would work. Unchanged from D7.

Everything else in the "impossible" pile is legally implementable.

### The real constraint is cost and verification, not law

What actually makes the long tail hard:

- **Someone must read FFmpeg's implementation and write a functional specification** before anyone can
  implement from it. Call it 0.5–3 person-weeks per format depending on complexity. Across ~490
  formats that is 250–750 pw of pure specification work *before* implementation begins.
- **Verification needs test media.** Without sample files for Delphine Software CIN, correctness is
  unprovable regardless of how good the implementation is.
- **Demand is near zero** for most of them. The long tail is long precisely because each entry serves
  very few users.

That is an economic argument for deprioritising them. It is not a legal argument for excluding them,
and the two must not be conflated — which is exactly the error being corrected.

### Reclassification

| Old | New | Meaning |
|---|---|---|
| T5 "cannot be done cleanly" | **T4-S — spec-extraction required** | Legally implementable. Needs a documented specification-extraction pass first, costed separately from implementation. Prioritise by user demand, never by legality. |
| — | **T5 — genuinely blocked** | Trained weights and non-functional authorial tables only. Expected to be a handful of components, not hundreds. |

Plans 15 and 18 must re-tier against this, and the roadmap's headline changes with it: v1.0's scope is
bounded by **effort and demand**, not by legal impossibility.

### The process that keeps risk low anyway

A strong legal position is not the same as avoiding a dispute, and a project without counsel should
prefer not to have the argument at all. The two-team split is cheap here and worth keeping:

1. **Spec extraction** produces a document describing the format: bitstream syntax, state machine,
   required tables, and the decode procedure. It is written to be implementable, and it contains no
   code, no comments, no identifier names and no function decomposition carried across from the
   source. The tables it records are those the format requires — recorded as data about the format,
   with that basis stated in the document.
2. **Implementation** works from that document only. Different person, and they do not open the
   reference source. This is the standard model and courts have accepted it.
3. **Provenance** per D7: every specification document names what it was derived from and by whom;
   every implementation PR cites the specification section it implements.

The cost of this is one extra person per format, and it converts a legal argument we would probably
win into one we most likely never have.

### Two things this does not change

- **Patents are untouched.** D4, D9 and the per-codec RED/AMBER/GREEN verdicts stand exactly as they
  are. Copyright analysis has no bearing on them, and the codecs that matter for patent risk
  (H.264, HEVC, VVC, AAC) all have public specifications anyway — they were never in this pile.
- **D7's rules on literal expression stand.** No code, no tables lifted from source without the
  functional-necessity basis established, no comments, no structure.

### Consequences

1. **The project's headline description was wrong and must be corrected everywhere it appears** —
   `planning/README.md`, the roadmap, and plans 15 and 18. "~50% cannot be clean-roomed" becomes
   "~50% requires a specification-extraction pass first and is deprioritised on demand".
2. **v1.0's definition improves.** It is bounded by effort and demand rather than by an
   impossibility, which means the long tail is a roadmap question the user controls, not a permanent
   hole in the product.
3. **`vaco-codec-exec` (shelling out to another tool) drops in priority.** It was designed as the
   answer to formats we could never support. Those formats are now merely expensive.
4. **Add a costed spec-extraction track to the roadmap** so long-tail formats can be pulled forward
   individually when demand justifies them.

### D12 second addendum — PF-0.0 measured (2026-08-21)

The adoption checklist was executed on real hardware. **The first addendum's
estimates were wrong, and wrong in our favour.** Corrected here because a
pessimistic forecast that stays uncorrected distorts every downstream decision as
badly as an optimistic one.

**Measured on:** Apple M5, `aarch64-apple-darwin`, rustc 1.97.1 / LLVM 22.1.6,
`fearless_simd` 0.7.0, bench profile. Min-of-100 over 500-pass samples, three runs
agreeing to ±0.01x, with `#[inline(never)]` symbols disassembled to confirm what
actually compiled.

**The headline: LLVM reconstructs the native instruction from our composition.**
The premise of the first addendum — that a missing operation means paying its
composition cost — is largely false on this target.

| Operation | 1st addendum estimate | Measured | What it compiled to |
|---|---|---|---|
| **Widening MAC, `pmaddwd` shape** | **~6x** | **0.79x** | `smull` + `smull2` + `addp.4s` — the optimal NEON form |
| **8-tap u8 FIR** | 2.2–2.5x | **1.12x** | |
| unsigned saturating add | 3 ops | **1.01x** | `uqadd.16b`, byte-identical to baseline |
| unsigned saturating sub | 2 ops | **1.01x** | `uqsub.16b` |
| integer abs | 2 ops | **1.00x** | `abs.8h` |
| absolute difference | 3 ops | **0.46x** | `uabd.16b` — *faster*; `u8::abs_diff` compiles worse |
| rounded average | 4 ops | **1.00x** batched | `urhadd.16b`; the gap was unrolling, not selection |
| horizontal reduction | ~2·log₂N | **0.99x** | with four accumulators; it was a latency chain |
| **signed saturating add/sub** | — | **1.46x** | the one genuine gap |

**A gap the plans missed, now the #2 upstream ask.** There is no `i16 → u8`
saturating narrow. The first addendum called `saturating_narrow` "the `packuswb`
we need"; it is not — `SimdNarrow` provides `i16→i8` and `u16→u8` only. Clamp to
0..255 and pack is the final step of essentially every pixel kernel we will write,
and it costs 2 extra operations there.

**Revised upstream asks:** (1) signed saturating add/sub, (2) `i16→u8` saturating
narrow, (3) widening MAC — worth raising, but say honestly that NEON does not hurt.
**Withdraw** the other five: they measure free.

**Two authoring rules worth more than any composition**, both invisible to
correctness tests and worth up to 4x:
- **Batch until you spill.** Batching helps until it does not — the FIR got *worse*
  when batched, one stack spill becoming six.
- **Never carry a single vector accumulator; use four.** The horizontal-reduction
  and rounded-average "gaps" were both latency chains, not missing instructions.

**Plan 11 §5.6's prescribed FIR structure is the worst variant.** "Hoist the
widen, `slide` per tap" measures 1.63x against 1.12x for the naive reload it was
meant to improve: twelve `ext.16b` all contend for one shuffle port. The plan is
amended.

**Other checklist results:** dispatch overhead 0.00–0.23 ns, indistinguishable
from a plain `fn` pointer. Inlining clean — the only call in any kernel body is a
cold bounds-check failure. NEON confirmed and asserted by a test. `dispatch!`
expands to no unsafe and `kernel!` does, confirmed at `kernel_macros.rs:226`.
Worked `yuv420p→rgb24` kernel: **4.1x over scalar** on a 1920px row.

#### Two things this does NOT settle

1. **Nothing here tests x86.** aarch64 has a single SIMD level, so the whole
   measurement is one target. x86 is where FFmpeg's assembly is densest and where
   `pmaddubsw`/`pmaddwd` matter most — so the risk the first addendum identified
   is *unmeasured*, not disproved. **Checklist item 7 (cross-tier bit-exactness)
   is blocking and cannot be closed on this hardware.** An x86-64 run, ideally
   AVX-512, is required before production kernels land. Treat the performance
   forecast as revised for aarch64 and still open for x86.
2. **These numbers depend on LLVM 22's combiner.** A toolchain bump could turn a
   1.00x row into 3x with no test failing. The crate's `probes` module exists to
   make this checkable; `xtask` should grow an instruction-selection assertion so
   a regression fails CI rather than being discovered in a benchmark months later.

**Verdict: adopt, confirmed.** On aarch64 the substrate costs essentially nothing.
The open question is x86, and it is a measurement we can schedule rather than a
design risk we must architect around — `vaco-simd` remains a D11 adapter, so even
a bad x86 result changes one crate's internals.

## D16 — `fd:` and numeric `pipe:` descriptors are out of scope (2026-08-22)

Raised by the `vaco-io` implementation. Plan 18 §2.4 P1 budgets an `fd:` protocol
at 0.2 pw and notes "Unix only (`OwnedFd`)". **That estimate assumes an escape
hatch D2 does not grant.**

Turning an integer into an owned file descriptor requires `FromRawFd::from_raw_fd`,
which is `unsafe` — and justifiably so. Nothing proves the integer names a
descriptor this process owns, and a wrong value closes someone else's socket when
the wrapper drops. That is not a formality; it is a real bug class that safe Rust
exists to prevent.

**Decision: `fd:` is not implemented, and `pipe:` supports only 0, 1 and 2.**
Those three work through `std::io::stdin`/`stdout`/`stderr`, which own their
descriptors legitimately. Any other `pipe:<n>` returns `Unsupported` naming the
reason.

The alternative — adding a `vaco-protocol-fd` crate to the D2 allowlist — is
rejected for now. D13 admits `unsafe` where it is the only way to reach hardware
that has no safe path; passing descriptors between processes is a convenience with
a shell-level workaround (`ffmpeg ... pipe:1 | vaco -i pipe:0`). The two are not
comparable, and the allowlist is only worth anything if it stays short.

Revisit only if a concrete workflow appears that genuinely cannot be expressed
through stdin/stdout.

## D17 — Where the reference is wrong, we match it anyway (2026-08-22)

The conformance harness found `vaco-core`'s colour table disagreeing with the
reference on two names. Verified directly:

| Name | Ours | FFmpeg 8.1 | SVG 1.1 / CSS Color 3 |
|---|---|---|---|
| `mediumpurple` | `#9370db` | `#9370d8` | **`#9370DB`** |
| `palevioletred` | `#db7093` | `#d87093` | **`#DB7093`** |

**We are right and the reference is wrong** — the same `db`→`d8` transposition in
both names, against a published standard.

**Decision: match the reference.** `-fill_color mediumpurple` must paint the same
pixels in both programs, and D6 makes byte-identical output the contract. A user
migrating a script cares that the output matches, not that we are more faithful to
a W3C document than the tool they are replacing. Being quietly *more correct* is
still a behavioural difference, and behavioural differences are what D6 exists to
eliminate.

The general rule this establishes, which will recur:

> Where the reference deviates from a published specification in **observable
> output**, we reproduce the deviation, record it as a known deviation with a
> citation, and report it upstream. Where the deviation is in something not
> observable, we follow the specification.

Both values are annotated in the colour table with the standard's value, the
reference's value, and why we chose the latter. This is not a bug to be "fixed"
later by someone reading the SVG spec — the annotation exists to stop exactly that.

### The converse case: where we are more permissive

We accept seven `grey`-spelled names the reference rejects (`grey`, `darkgrey`,
`dimgrey`, `slategrey`, `darkslategrey`, `lightslategrey`, `lightgray`), making our
set a strict superset of theirs — 147 against 140. The reference's own spelling is
internally inconsistent, using `Gray` for six pairs and `LightGrey` for the seventh.

**We keep the superset.** Being more permissive breaks nothing: every command that
works against the reference works against us. The reverse — a script written
against us failing on the reference — is a real but minor cost, and the
alternative is deliberately rejecting valid CSS colour names to reproduce an
inconsistency. Documented in the crate docs so the asymmetry is not a surprise.

### D17.1 — Deviations we *cannot* reproduce, because a type forbids them (2026-08-22)

D17 says to match the reference where it deviates from spec in observable
output. `vaco-chlayout` found the first case where that is impossible.

The reference truncates a channel label at 15 **bytes**, without regard to
character boundaries. Probed byte-exactly:

```
-ch_layout "FL@ééééééééé+FR"   ->   FL@ + c3a9 c3a9 c3a9 c3a9 c3a9 c3a9 c3a9 c3
                                          seven é (14 bytes) + a dangling lead byte
```

That output is **not valid UTF-8**. A Rust `String` cannot hold it, so
`describe() -> String` cannot reproduce it — not as a matter of effort, but by
the type's guarantee. And every lossy rendering of the dangling byte re-parses
to a third value, breaking the `describe → parse → describe` fixed point the
whole layout grammar rests on.

**The rule.** When byte-identical output would require constructing a value a
Rust type forbids:

1. **Do not weaken the type** to accommodate it. `String`'s guarantee is worth
   more than the last byte of a pathological label.
2. **Diverge minimally and predictably** — here, cut at the last character
   boundary at or below the reference's byte cap. Inputs that do not hit the
   pathological case, which is every ASCII label and every realistic command
   line, stay byte-identical.
3. **Pin the divergence in a test that asserts it still exists**, so that if the
   reference changes, or we find a way to close it, the test tells us instead of
   silently passing. `vaco-chlayout` uses `LABEL_TRUNCATION_DIVERGENCE` for
   exactly this, and the same pattern retired its earlier label gap when that one
   became closable.
4. **Record it in the crate's doc file** under a heading someone auditing
   conformance will find, not only in a code comment.

This is a narrower exception than it looks. It applies when a *type invariant*
makes reproduction impossible, not when reproduction is merely awkward,
unpleasant, or requires `unsafe`. If the only obstacle is effort, D17 stands and
we match the reference.

#### A note on how this was found

The first probe of this used `grep -oE` to extract the label, and `grep` silently
dropped the dangling byte — so the output looked like clean UTF-8 and the whole
divergence was invisible. It took re-probing with byte-exact tooling to see it.
That is plan 13 §1b's rule applying to the *read* side as well as the write
side: the tool between you and the answer has opinions. When the question is
about bytes, look at bytes.

## D18 — Vaco must compile for wasm32, so OS coupling lives behind one door (2026-08-22)

**Decision.** Every Vaco *library* builds for `wasm32-unknown-unknown`. Where a
platform capability is not available there, it is abstracted into a single crate
rather than `#[cfg]`-ed at each use site. `cargo xtask wasm-check` enforces it,
and CI runs it.

This is a forward-looking constraint: there is no wasm product yet. It is
adopted now because retrofitting portability after the I/O and threading layers
harden is enormously more expensive than keeping it, and because the discipline
it imposes — knowing exactly where the OS is touched — is worth having anyway.

### Measured starting position

Better than expected. At adoption, **all 27 libraries already built for wasm**,
including `vaco-io` and `vaco-format-core`. The whole cost of D18 was one new
crate and three call sites.

### The one thing that actually breaks

`std::time::Instant::now()` and `SystemTime::now()` do not merely fail on
`wasm32-unknown-unknown` — they **panic**. In a workspace that `deny`s
`unwrap_used`, `expect_used` and `panic` specifically to keep untrusted input
away from a crash, a panicking clock is the worst available failure mode.

Note what does *not* break: `std::fs` compiles on wasm and returns runtime
errors. Graceful failure is fine. The rule is about panics and compile failures,
not about whether an operation can succeed.

### `vaco-time`

The clock now lives in `crates/core/vaco-time` and nowhere else, exposing:

- `Instant` — monotonic, present on every target, saturating rather than
  panicking on the argument orders `std` panics on.
- `unix_nanos() -> Option<u128>` — wall clock, `Option` because a target
  genuinely may not have one.

They are deliberately **different types**, because they answer different
questions and have different availability. `Instant` is deliberately not
`std::time::Instant`, so a crate cannot silently reacquire the dependency.

On wasm without the `web` feature, `Instant` is a stopped clock and
`unix_nanos` returns `None`. Each fallback is chosen so the failure is safe: a
deadline set forward never fires (deadlines are a last-resort guard, and the
byte and element budgets that actually bound an attacker are untouched); a seed
falls back to a constant, which weakens nothing, since `parse::color("random")`
is documented as carrying no statistical claim; `time` in an expression reads 0.

The intended fix is the declared-but-unwired `web` feature backed by `web-time`.
It is not wired yet because adopting a dependency before there is a wasm build
to test it against is adoption on faith, and D10 makes every adoption reviewed.

### The allowlist default is the point

`wasm-check` treats a crate as portable **unless** it is on `NATIVE_ONLY`. A new
crate is portable by default, and making one native-only is a deliberate act
that leaves a written reason — the same shape as the unsafe audit's exemptions.
The opposite default would let OS coupling spread silently, which is the failure
D18 exists to prevent.

One entry today: `vaco-protocol-http`, which is `ureq` + `rustls` — a
socket-and-TLS stack, and `wasm32-unknown-unknown` has no sockets. Note this is
**not** a hole in the design. Portability there means a *different* protocol
implementation behind the same `vaco-protocol-core` trait, which is the D11
adapter rule working exactly as intended.

### Scope

- **Libraries only.** We do not run the test suite on wasm: `proptest` pulls
  `rand_core` and `tempfile` pulls `getrandom`, which `compile_error!`s on wasm
  without its `js` feature. Both are dev-dependencies — `cargo tree -e normal -i
  getrandom` finds nothing — so no shipped library is affected. This is also why
  the gate builds each library individually instead of `--workspace`, which
  resolves one unified feature graph and drags dev-dependencies in.
- **Threading is not yet addressed.** `wasm32-unknown-unknown` has no threads.
  Nothing in the tree spawns any today, so the question is open rather than
  answered; when `vaco-sched` lands it must make parallelism optional at the
  API level, not merely feature-gated at the call site.
- D16 already excluded `fd:` and numeric `pipe:` for needing `from_raw_fd`,
  which points the same way.

### D17.2 — Reference defects we do not reproduce because the reference's own output is undefined (2026-08-22)

D17 says match the reference where it deviates from spec in observable output.
D17.1 carved out the case where a Rust type forbids constructing the value.
`vaco-scale` found a third case, and it needs its own rule.

Converting Y'CbCr to RGB, the reference emits **0** where the pre-clip value
reaches 512 — the first index past the end of its clipping table. Reproduced
directly:

```
yuv444p 1x1, scale=in_range=tv:out_range=pc:in_color_matrix=bt709
  Y=224 U=255 V=128  ->  B = 255      correct saturation
  Y=225 U=255 V=128  ->  B =   0      one past the table
  Y=226 U=255 V=128  ->  B =   0
```

It is a sharp cliff, not a drift, and it is reachable from ordinary out-of-gamut
chroma — not a contrived input.

**We saturate. We do not reproduce it.** The rule:

> Where the reference's behaviour is a **read of memory it does not own**, the
> observable output is a property of one build's memory layout rather than of
> the reference. There is nothing stable to match, so match the specification
> instead, and pin the divergence.

This is narrower than "the reference has a bug". D17 stands for bugs with
defined, reproducible output — the `image_size` truncation and the `bt470m`
name tables are both reproduced faithfully. The distinction is whether repeating
the behaviour requires repeating an out-of-bounds access. It does here: emitting
0 is not a computation we could justify, it is whatever that build's linker put
after the table.

Three obligations come with the exception, all met by `vaco-scale`:

1. **Pin it against the live reference**, not against a recorded constant, so
   the test reports if the reference's behaviour ever changes.
2. **Pin the value either side of the cliff**, proving it is a cliff and
   bounding exactly which inputs diverge.
3. **Expose it as a named constant** (`REFERENCE_CLIP_DIVERGENCE`) and document
   it where a conformance audit will look, not only in a code comment.

Note what this does *not* license. "The reference is wrong and we know better"
is not a reason to diverge — D17 exists precisely to overrule that instinct.
The test is narrow: undefined behaviour in the reference, no stable output to
match, divergence pinned and documented.

### D18.1 — a compile gate cannot see a runtime panic (2026-08-22)

`wasm-check` answers "does this compile for wasm32?", and that is a weaker
question than it looks. `std::time::Instant::now()` **compiles** for
`wasm32-unknown-unknown` and panics when called. So a crate can pass the gate on
every run and still be unusable on the target it just passed for — and in a
workspace that denies `unwrap_used` and `panic` specifically to keep untrusted
input from reaching a crash, a panic that no lint and no gate can see is the
worst shape available.

Two crates were in exactly that state, found by reading rather than by any gate:

| where | what |
|---|---|
| `vaco-protocol-file` | the `follow` read built its deadline with `std::time::Instant` and polled with `std::thread::sleep` |
| `vaco-protocol-core` | `DirEntry.modified` was `Option<std::time::SystemTime>` |

The second is the more instructive. It is a field in a **trait's data model**,
so it obliged every implementer of `Protocol` to produce an OS type — coupling
in the *interface*, where no `cfg` can reach it. It is now `Option<i64>`
microseconds since the epoch. Signed, because filesystems carry pre-1970
timestamps.

`cargo xtask time-gate` now enforces the rule, with a crate allowlist (three
entries) and a line-level `// time-gate: <reason>` escape hatch for converting a
value the OS has already handed you. `Duration` is deliberately **not** flagged:
it is arithmetic over two integers, `vaco_time` re-exports
`core::time::Duration`, and the two spellings name the same type. A gate that
fires on `Duration` is a gate people learn to ignore.

#### `vaco-time` alone would not have fixed the `follow` loop

Worth recording because it generalises. The loop was
`while Instant::now() < deadline`. Swapping in `vaco_time::Instant` removes the
panic — and replaces it with a **hang**, because that `Instant` is a *stopped*
clock where there is no monotonic source, so the condition stays true forever.

The abstraction moved the failure; it did not remove it. The loop shape had to
change too, and is now bounded by an iteration count as well as by the deadline,
with the count derived from the same two durations so that on a working clock it
never truncates a wait. `vaco_time::sleep` is now the third door alongside
`Instant` and `unix_nanos`, and `can_sleep()` exists so a caller can ask.

**The transferable rule: a polling loop must be bounded by a count, not only by
a clock.** Putting a capability behind one door tells you where the OS is
touched; it does not tell you that the code above the door was written assuming
the capability works.

## D19 — One definition per concept, enforced (2026-08-22)

**Decision.** Every concept has exactly one definition. Where a thing is needed
by two crates, it moves down to a shared home rather than being written twice.
`cargo xtask dup-check` enforces it and CI runs it.

This generalises what D11 already says about external crates ("reachable from
exactly one Vaco crate") to our own types, and what §3.4 says about generated
files ("a crate declares what it provides; the shared file is derived").

### Why a gate rather than an audit

The first manual pass over this workspace **missed the one type already known to
be duplicated.** `vaco_format_core::Disposition` is declared inside a
`bitflags!` invocation, so it is indented, and a `^pub struct` search never sees
it. An audit that misses the case you already know about is not an audit.

So `dup-check` runs in CI over two explicit lists:

- **`DISTINCT`** — names that appear twice and mean different things, each with
  the reason. `Tier` is a SIMD tier and an HEVC tier; `Component` is a pixel
  component and a registered component. Writing the reason down is the point: it
  is what stops the list becoming a place to hide real duplication.
- **`KNOWN_DUPLICATE`** — the same concept twice, tracked with a resolution plan
  so it cannot be forgotten and cannot grow quietly.

A new shared name that is on neither list fails the build.

### What it cannot do

It compares **names**, which is a proxy for concepts. It cannot see two types
that mean the same thing under different names, and it cannot see a duplicated
*table* or constant. Those still need a person. It is a ratchet against
regression, not a proof of non-duplication.

### Outstanding, with plans

| name | crates | plan |
|---|---|---|
| `CancelToken` | `vaco-io`, `vaco-codec-core` | Both are `Arc<AtomicBool>` with different semantics on top. Shared primitive belongs in `vaco-core`; neither crate depends on the other today. |
| `Disposition` | `vaco-cli-core`, `vaco-format-core` | Aligned numerically (19 flags, same bits) so nothing is wrong today. cli-core does not depend on format-core, so the shared home has to sit below both. |

#### `OptFlags` was on that list and should not have been (resolved 2026-08-22)

The entry above read "the easy one" and was wrong twice over. It claimed
cli-core's version "adds a column concept for `-h full` rendering, which
`vaco-opts` should carry" — but `vaco-opts::OptFlags` is *already* the one with
`column()`, and cli-core's never renders a column at all.

Probing settled it. `ffmpeg -h full` prints an eleven-column flag field beside
every `AVOption`:

```
  -b   <int64>   E..VA...... set bitrate (in bits/s)
```

and prints **no flag column** beside the command-line options above it:

```
-muxers             show available muxers
```

Two flag universes on two structures. `vaco-opts::OptFlags` is the column —
encoding, decoding, filtering, readonly, runtime, deprecated. cli-core's is
argv mechanics — consumes the next entry, global or per-file, binds to an input
or an output. Even the three spellings they share diverge: `VIDEO`/`AUDIO`/
`SUBTITLE` are media applicability in one and help *grouping* in the other.

So it belonged in `DISTINCT`, not `KNOWN_DUPLICATE`. But leaving it as a shared
name was still wrong, because `vaco-cli-core` depends on `vaco-opts` and
`-h full` needs both sets **in the same file** — the one place where two types
of the same name is not a documentation problem but a `use` conflict. cli-core's
is now `ArgFlags`, and the collision is gone rather than explained.

The transferable lesson is about the list itself: a `KNOWN_DUPLICATE` row is a
claim, and claims written from a name-based scan are exactly as unreliable as
the scan. Before merging a tracked duplicate, check that it *is* one. Two of
the three rows on this list were written the same way; treat the remaining two
as unverified.

The H.264/HEVC pair share **21 type names** (`Sps`, `SliceHeader`,
`HrdParameters`, `NalUnitType`, …). Those are *not* duplication today: the two
standards define structurally different syntax for the same concepts.
`vaco-codec-cbs` exists to unify what can be unified, and its author was asked
directly whether its representation serves H.264 as well as HEVC. That answer
decides which of the 21 collapse; until then they stay, marked `(D19: cbs)`.

### A case that was already right

`vaco_pool::BITSTREAM_PADDING` and `vaco_bitstream::Padded::PAD` are both 64 and
in different crates — but `vaco-pool` depends on `vaco-bitstream` and carries a
`const _: () = assert!(…)` tying them together, with a comment saying a compile
error means one of the two moved. That is the shape to copy when a constant
genuinely must live in two places: not a comment saying they match, a compile
error when they stop.
