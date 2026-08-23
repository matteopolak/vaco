# 20 — The Integrated Roadmap

> **AMENDED BY D15 (post-authoring).** This roadmap was written before D15 corrected the
> "cannot be clean-roomed" finding. Every reference below to formats being permanently out of reach
> should be read as **"requires a specification-extraction pass first, prioritised on demand"** —
> they are legally implementable. R1 in §6 has been rewritten; other incidental references have not.
> The effort and calendar totals are unaffected, since the long tail was already out of v1.0 scope.
> D15 adds a **costed spec-extraction track** (0.5–3 pw per format) that is not yet in the register.


**Status:** the single executable schedule. Merges plans 11–18 into one register, one critical path,
one set of milestones, and one wave schedule under the execution constraints of `19-parallel-execution.md`.
Binding constraints come from `00-decisions.md` (D1–D14). Where the domain plans disagree, §9 adjudicates.

This document is the orchestrator's assignment source. It is meant to be executed tomorrow, not admired.

**Headline, stated once, plainly:**

| Question | Answer |
|---|---|
| Deduplicated effort, everything the nine plans describe (T1 codecs only) | **1,418 person-weeks ≈ 27 person-years** |
| Deduplicated effort to **v1.0** as §4 defines it | **~1,680 person-weeks ≈ 32 person-years** |
| Dependency-critical path to v1.0 | **~53.5 calendar weeks** *(was 79.5 with T3-01/T3-02 unsplit; §3.3, §11)* |
| Actual calendar to v1.0 at a sustained 4–8 agents | **~336 weeks ≈ 6.5 years** (range 5.4–10.1) |
| Calendar to v0.1 (`vaco-probe`, D5 as amended by D14.4) | **~50 weeks ≈ 11.5 months** |

The gap between 53.5 weeks of critical path and 336 weeks of calendar is the whole story: **this project
is throughput-bound, not dependency-bound.** The architecture has already done its job — there is
essentially always an unblocked crate to hand a free agent. The binding constraint is the 4–8 agent
hardware ceiling in `19-parallel-execution.md` §7, and nothing in the plans changes it.

---

## Table of contents

1. [The unified work-package register](#1-the-unified-work-package-register)
2. [Reconciling the totals](#2-reconciling-the-totals)
3. [The critical path](#3-the-critical-path)
4. [Milestones and acceptance criteria](#4-milestones-and-acceptance-criteria)
5. [The wave schedule](#5-the-wave-schedule)
6. [Risk register](#6-risk-register)
7. [The first ten work packages](#7-the-first-ten-work-packages)
8. [Open questions requiring a decision](#8-open-questions-requiring-a-decision)
9. [Where the plans disagree, and how it is adjudicated](#9-where-the-plans-disagree-and-how-it-is-adjudicated)
10. [What this roadmap deliberately does not do](#10-what-this-roadmap-deliberately-does-not-do)
11. [The decomposition pass: what was split, and why](#11-the-decomposition-pass-what-was-split-and-why)

---

## 0. ID prefixes

Prefixes already assigned by the domain plans are preserved verbatim. Prefixes are invented only where
a plan numbered its work without one.

| Prefix | Origin | Meaning |
|---|---|---|
| `P0-` | plan 19 §6 | Phase 0, contract-first. **New** — plan 19 describes it but never packaged it. |
| `FD-` | plan 11 | Foundations (layer 0/1 crates). **New prefix**; plan 11 identified work by crate name. |
| `PF-` | plan 12 | Performance. **New prefix**; plan 12's own `0.1`/`3.2` numbering is preserved after it. |
| `QA-` | plan 13 | Correctness infrastructure. **New prefix**; plan 13 carried no IDs and no estimates. |
| `CL-` | plan 14 | CLI layer. **New prefix**; plan 14 identified work by table row. |
| `F-` `D-` `P-` `C-` `X-` `B-` `H-` `T2-` `T3-` `T5-` | plan 15 | Preserved verbatim. |
| `FT-` | plan 16 | Filters. **New prefix**; plan 16's phase numbering (`1.1`, `4.12`) is preserved after it. |
| `SP-A` `SP-B` `SP-C` | plan 17 | Signal processing. **New prefix**; plan 17's `A1`/`B1`/`C1` numbering preserved. |
| `FW-` `IO-` `PR-` `SH-` `FM-` `XF-` | plan 18 | Preserved verbatim. |

Two near-collisions worth naming so nobody trips: plan 15's `F-01` (`vaco-codec-core`) is **not**
plan 18's `FW-01` (`vaco-format-core`), and plan 15's `X-0n` (harness packages inside the codec plan)
is **not** plan 18's `XF-0n` (format conformance). Both pairs are load-bearing and both appear on the
critical path.

---

## 1. The unified work-package register

**Effort column convention.** `pw` is the plan's stated figure. Where §2 removes double counting, the
row carries the *net* figure and a `[dedup]` note naming what was removed and to whom it was
reassigned. Rows struck to zero are kept, not deleted, so the arithmetic is auditable.

**Wave** maps to `19-parallel-execution.md` §7. See §5 for what each wave means and when it opens.

**Reading a split package.** After the §11 decomposition pass, a row whose ID is bold and marked
*(group)* is **not dispatchable**. It is the parent ID kept as a grouping label so the register stays
navigable and so the parent's total remains auditable; the dispatchable work is the lettered children
immediately below it. Every child carries an **Acc:** clause — the acceptance check that proves *that
child* is done, independently of its siblings. **A dependency written as a parent ID means all of that
parent's children**; where a narrower child dependency is what actually gates the work, the child ID is
named explicitly, and doing so is what shortened three chains in §3.3.

**Subtotals are unchanged by the decomposition.** Children sum exactly to their parent in every case
(§11.6); no estimate was revised. The register total is still 1,782.0 pw and the v1.0 planning case is
still ~1,680 pw.

### 1.1 Wave 0 — the contract (`P0-`, plan 19 §6)

Plan 19 describes Phase 0 as "2–3 weeks of serial work" and never breaks it down. It is broken down
here because it is the one part of the project that cannot be handed to more than one or two agents,
and an unpackaged critical-path phase is how schedules quietly lose a month.

| ID | Title | Crate(s) | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| P0-01 | Root manifest (glob members, `[workspace.dependencies]` pre-populated per plan 11 §1), `rust-toolchain.toml` (stable 1.89 per D12), `clippy.toml`, `rustfmt.toml`, profiles, `deny.toml`, `about.toml`, `.cargo/config.toml` with the sccache wrapper | *(orchestrator-only files)* | 19 §3, 11 §1 | 1.0 | — | 0 |
| P0-02 | Every crate directory in `10-architecture.md` §3 created with its manifest and dependency edges; crate graph real and acyclic; `layers.toml` written **with D14.1's correction applied** | ~120 crate dirs | 19 §6.2 | 1.0 | P0-01 | 0 |
| P0-03 | Every public type, trait and fn signature written out and compiling, bodies `todo!()`: `vaco-core` errors/rationals, `Frame`/`Packet`, `Demuxer`/`Muxer`, `Decoder`/`Encoder`/`Parser`/`BitstreamFilter`, `Filter`/`Activity`, `vaco-opts` derive shape, `KernelSet`. **Then the interfaces freeze.** | all | 19 §6.3 | 1.5 | P0-02 | 0 |
| P0-04 | `xtask` skeleton (`gen-registry`, `gen-docs-index`, `layer-check`, `dep-gate`), `Justfile` with the `--target-dir` flag form baked in, CI pipeline green on `cargo check --workspace` + `cargo doc` | `xtask` | 19 §3–4, 13 §0.2 | 1.0 | P0-01 | 0 |
| P0-05 | `planning/ASSIGNMENTS.md`, the agent task-contract template (19 §8), the orchestrator's wave-boundary checklist | `planning/` | 19 §8 | 0.5 | P0-03 | 0 |
| | **Wave 0 subtotal** | | | **5.0** | | |

### 1.2 Foundations (`FD-`, plan 11 §17)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| FD-01 | Error/Result taxonomy, `Rational`, timestamps and time bases, rescaling with explicit rounding modes, `MediaType`, `tracing` façade | `vaco-core` | 11 §4 | 2.0 | P0-03 | 1 |
| FD-02 | Buffer pooling with 64-byte alignment guarantees | `vaco-pool` | 11 §15 | 1.0 | FD-01 | 1 |
| FD-03 | Bit reader/writer, Exp-Golomb, byte readers, start-code scanning; checked-tail / unchecked-body split | `vaco-bitstream` | 11 §8 | 2.5 | FD-01 | 1 |
| FD-04 | ~~`vaco-simd` substrate~~ **[dedup −3.0 → PF-0.1]** | `vaco-simd` | 11 §5 | 0.0 | — | 1 |
| FD-05 | `Options` derive: typed, introspectable, string-parsable option sets with ranges, defaults, units, named constants, runtime-settable flags; `-h full` differential harness **first**, macro second | `vaco-opts`, `vaco-opts-derive` | 11 §6 | 5.0 | FD-01 | 1 |
| FD-06 | The `eval` expression language: constants, unary math, `st`/`ld`, `while`/`taylor`/`root`, `if`/`ifnot`, `between`/`clip`, infix ops; `aevalsrc` oracle pins the semantics | `vaco-expr` | 11 §7 | 2.5 | FD-01 | 1 |
| FD-07 | ~268 pixel formats + descriptor metadata, **generated** from a declarative table; differential extractor is the acceptance criterion and is written first | `vaco-pixfmt` | 11 §9 | 3.0 | FD-01 | 1 |
| FD-08 | Sample formats, planar/interleaved, byte widths **+ the channel-layout model** (`Unspecified`/`Native`/`Custom`/`Ambisonic`, identifiers, named layouts, parse and display) — **FD-09 merged in, §11.5** | `vaco-sampfmt`, `vaco-chlayout` | 11 §10–11 | 2.0 | FD-01 | 1 |
| FD-09 | ~~Channel-layout model~~ **[merged → FD-08, §11.5]** | `vaco-chlayout` | 11 §11 | 0.0 | — | 1 |
| FD-10 | Primaries, transfer, matrix, range, chroma location, alpha mode (ITU-T H.273) + chromaticity data + matrix derivation | `vaco-color` | 11 §12 | 2.0 | FD-01 | 1 |
| FD-11 | `Frame` (video + audio), plane storage, strides, the full `SideData` set, metadata, cropping; `Arc`-shared CoW buffers | `vaco-frame` | 11 §13 | 3.0 | FD-07, FD-08, FD-02 | 1 |
| FD-12 | `Packet`, packet side data, timestamps, flags | `vaco-packet` | 11 §14 | 1.0 | FD-01, FD-02 | 1 |
| | **Foundations subtotal** | | | **24.0** *(plan states 27.0)* | | |

### 1.3 Performance (`PF-`, plan 12 §8)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| PF-0.0 | **`fearless_simd` adoption checklist** (plan 12 §11): the `pmaddubsw` composition measured with `llvm-mca`, all nine gap compositions, dispatch overhead, binary size, inlining proof, cross-tier bit-exactness | *(spike, no crate)* | 12 §11 | 0.5 | — | 0 |
| PF-0.1 | `vaco-simd` adapter (D12): `Tier`, `Variant`, `KernelSlot`, `CpuProfile`, `dispatch_kernel!`, `ops` module with all nine gap compositions, each with an exhaustive test and an instruction-count assertion. **The D11 boundary: `fearless_simd` is named here and nowhere else.** | `vaco-simd` | 12 §8, 11 §5 | 3.0 | PF-0.0, P0-03 | 1 |
| PF-0.2 | `vaco-checkasm` core: `Kernel` trait, `Differential` builder, edge-case generators, mismatch reporting, CLI verify mode | `vaco-checkasm` | 12 §8, 15 X-02, 17 D.1.1 | 3.0 | PF-0.1 | 1 |
| PF-0.3 | `vaco-checkasm` bench mode: `perf-event` backend, `Instant` fallback, nop baseline, hot/cold protocol, JSONL + baseline compare | `vaco-checkasm` | 12 §8 | 2.0 | PF-0.2 | 1 |
| PF-0.4 | `vaco-vecheck`: remark parsing, `#[vaco::must_vectorize]`, `vecheck.toml`, `cargo-show-asm` assertions, waiver expiry | `vaco-vecheck` | 12 §8 | 2.0 | PF-0.1 | 1 |
| PF-0.5 | `vaco-bench` macro harness + `vaco-corpus` manifest/fetch/verify + machine-control preconditions | `vaco-bench` | 12 §8 | 3.0 | — | 1 |
| PF-0.6 | Results store: JSONL schema, `bench-results` branch tooling, HTML report, CI regression gates | `vaco-bench` | 12 §8 | 2.0 | PF-0.3, PF-0.5 | 1 |
| PF-0.7 | ~~`VACO_TIER` override + binary-size budget check~~ **[merged → PF-4.6, §11.5]** | `vaco-simd`, CI | 12 §8 | 0.0 | — | 4 |
| PF-0.8 | PGO pipeline: `just pgo-build`, `vaco-profile`, workload manifest, coverage/anti-overfit checks, profile lockfile + refresh job | `xtask`, CI | 12 §8 | 3.0 | PF-0.5 | 5 |
| PF-0.9 | BOLT pipeline (Linux x86-64), `--emit-relocs` plumbing, re-verification job | CI | 12 §8 | 1.0 | PF-0.8 | 5 |
| PF-1.x | ~~Colour/pixel conversion, packed↔planar, swscale h/v filters, audio format conv + rematrix, polyphase, dither kernels~~ **[dedup −22.5 → SP-A9/A10, SP-B3/B6/B9]** | `vaco-scale`, `vaco-resample` | 12 §8 | 0.0 | — | 2 |
| PF-2.1 | Deinterlace kernels: `bwdif`, `yadif`, `w3fdif`, `estdif` — SIMD beyond what plan 16 budgets | `vaco-filter-deinterlace` | 12 §8 | 2.0 *[dedup −2.0 → FT-4.5]* | FT-4.5 | 4 |
| PF-2.2 | Blur/sharpen kernels: `gblur`, `boxblur`, `unsharp`, `smartblur` | `vaco-filter-blur` | 12 §8 | 1.5 *[−1.5 → FT-4.6]* | FT-4.6 | 4 |
| PF-2.3 | Denoise kernels: `hqdn3d`, `atadenoise`, `removegrain`, `nlmeans` | `vaco-filter-denoise` | 12 §8 | 2.5 *[−2.5 → FT-4.6]* | FT-4.6 | 4 |
| PF-2.4 | Compositing/blend kernels: `overlay`, `blend`, `alphamerge`, `colorkey` | `vaco-filter-overlay` | 12 §8 | 1.5 *[−1.5 → FT-4.11]* | FT-4.11 | 4 |
| PF-2.5 | Analysis kernels: `ssim`, `psnr`, `colordetect`, `signalstats` | `vaco-filter-analysis` | 12 §8 | 1.5 *[−1.5 → FT-4.9]* | FT-4.9 | 4 |
| PF-2.6 | `lut3d` / colour management / tonemap SoA lattice + tetrahedral interpolation | `vaco-filter-color` | 12 §8 | 2.0 *[−2.0 → FT-4.4]* | FT-4.4, SP-A6 | 4 |
| PF-2.7 | Audio filter kernels: `atempo`, `loudnorm`, `firequalizer`, `aresample` glue | `vaco-filter-adsp` | 12 §8 | 1.5 *[−1.5 → FT-4.8]* | FT-4.8 | 4 |
| PF-3.1 | Shared `vaco-codec-dsp-*` SIMD: `idct`, `hpel`, `videodsp` edge emulation, `blockdsp`, `fmtconvert` (~40 kernels) | `vaco-codec-dsp-*` | 12 §8 | 1.5 *[−4.5 → D-05/D-11]* | D-05, D-11 | 4 |
| PF-3.2 | **H.264 MC**: qpel luma, chroma, weighted pred, **batched dispatch** (§1.4 Risk C — changes the `Decoder`↔`KernelSet` contract, must settle before F-02 freezes) | `vaco-codec-dsp-mc` | 12 §8 | 3.0 *[−5.0 → D-08]* | F-01, PF-0.1 | 2 |
| PF-3.3 | H.264 deblock + intra pred SIMD | `vaco-codec-dsp-deblock/-intrapred` | 12 §8 | 1.5 *[−4.5 → D-09/D-10]* | D-09, D-10 | 4 |
| PF-3.4 | ~~`vaco-tx` SIMD~~ **[dedup −8.0 → SP-C8/C9]** | `vaco-tx` | 12 §8 | 0.0 | — | 2 |
| PF-3.5 | HEVC/VVC epel/qpel + SAO + deblock + ALF SIMD (~90 kernels) | `vaco-codec-hevc` | 12 §8 | 4.0 *[−8.0 → T3-02]* | T3-02 | 4 |
| PF-3.6 | VP9/AV1 inverse transforms incl. 10/12-bit (~50 kernels) | `vaco-codec-vp9/-av1` | 12 §8 | 3.0 *[−7.0 → C-30/C-40]* | C-30, C-40 | 4 |
| PF-3.7 | VP8/VP9 MC, intra pred, loop filter SIMD (~55 kernels) | `vaco-codec-vp8/-vp9` | 12 §8 | 2.0 *[−6.0 → C-16/C-31/C-32]* | C-16, C-32 | 4 |
| PF-3.8 | AV1 reconstruction SIMD: MC, CDEF, loop restoration, film grain (~70 kernels) | `vaco-codec-av1` | 12 §8 | 4.0 *[−8.0 → C-39/C-41/C-42]* | C-41 | 4 |
| PF-3.9 | `me_cmp` SIMD: SAD, SATD, SSD, variance | `vaco-codec-dsp-mecmp` | 12 §8 | 1.0 *[−3.0 → D-12]* | D-12 | 4 |
| PF-3.10 | Encoder DSP SIMD: forward transforms, quantisation, RDO helpers, `lpc` | `vaco-codec-dsp-*` | 12 §8 | 1.5 *[−4.5 → D-07/D-14]* | D-14 | 4 |
| PF-3.11 | AAC SBR/PS QMF, `sinewin`, `ac3dsp` SIMD | `vaco-codec-aac/-ac3` | 12 §8 | 1.5 *[−3.5 → T3-03/T2-04]* | T3-03 | 4 |
| PF-4.1 | Bit reader design: 64-bit refill, branchless renormalisation, `#[inline(always)]` discipline | `vaco-bitstream` | 12 §8 | 1.5 *[−1.5 → FD-03]* | FD-03 | 2 |
| PF-4.2 | CABAC engine: cache-line-aware state table layout, branch-free renormalisation, batched bypass decode | `vaco-codec-cabac` | 12 §8 | 2.0 *[−2.0 → D-03]* | D-03 | 4 |
| PF-4.3 | CAVLC / Exp-Golomb table layout and lookup-width tuning | `vaco-codec-vlc/-golomb` | 12 §8 | 1.0 *[−1.0 → D-01/D-02]* | D-01 | 4 |
| PF-4.4 | PGO profile tuning for entropy paths; BOLT layout verification | CI | 12 §8 | 2.0 | PF-0.9 | 5 |
| PF-4.5 | Container demux hot paths: MPEG-TS packet loop, EBML parse, MP4 sample-table walk | `vaco-demux-*` | 12 §8 | 2.0 *[−1.0 → FM-01/07/10]* | FM-11 | 4 |
| PF-4.6 | Startup latency: lazy registry construction, table-generation avoidance, binary-size / page-in cost **+ the `VACO_TIER` override and the binary-size budget check in CI** — **PF-0.7 merged in, §11.5** | `vaco-registry`, `vaco-simd`, CI | 12 §8 | 2.5 | PF-0.1 | 5 |
| | **Performance subtotal** | | | **66.0** *(plan states 168.5)* | | |

### 1.4 Correctness (`QA-`, plan 13)

Plan 13 is the only domain plan with **no effort estimates at all**. The figures below are this
document's, derived from the scope in plan 13 §0.2, §1–§7 and §9, and are the largest single piece of
new estimation in this roadmap. They cover only what is *not* already budgeted as `X-0n` (plan 15),
`XF-0n` (plan 18), `FT-6.x` (plan 16) or `SP-A13`/`SP-B13`/`SP-C11` (plan 17).

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| QA-01 | `vaco-limits`: the resource-budget type threaded through every component touching untrusted input, plus the clippy allocation bans. **Must exist before the first demuxer** — retrofitting a required constructor parameter across 90 crates is a change nobody ever does. | `vaco-limits` | 13 §0.2, §2.2 | 1.5 | FD-01 | 1 |
| QA-02 | `vaco-conformance` beyond X-01's comparator core: manifest loader, matrix expansion, case ids, normalisation chain, divergence-allowlist engine with the ratchet, reporter, `conformance-explore`/`-run` | `vaco-conformance` | 13 §1 | 5.0 | X-01 | 1 |
| QA-03 | Reference pinning (`stable` + `next`), the refbin container, `refbin-bump` with the dual-run report and the follow/hold/intentional-change taxonomy | `vaco-conformance` | 13 §1.9 | 3.0 | QA-02 | 1 |
| QA-04 | `vaco-corpus` beyond X-05: content-addressed object store, `vaco-media.lock`, mutate, minimise | `vaco-corpus` | 13 §2.5 | 3.0 | X-03 | 1 |
| QA-05 | `vaco-fuzz-support` (`Guard`, `ProgressGuard`, structured input types, corpus-replay) + `vaco-fuzz-alloc` (counting `GlobalAlloc`, D2 allowlist entry + the CI assertion it never reaches a shipped artifact) + `fuzz-regressions` replay as a normal test | `vaco-fuzz-support`, `vaco-fuzz-alloc` | 13 §2 | 3.0 | X-03, QA-01 | 1 |
| QA-06 | `xtask` verification jobs: `layer-check` (with D14.1 applied), `unsafe-audit`, `provenance-check`, `docs-check`, `assert-release-features`, the D9 `*-sys` manual-review gate | `xtask` | 13 §5, §7 | 3.0 | P0-04 | 1 |
| QA-07 | CI design: the PR stage, the nightly matrix, sanitizer jobs for `vaco-hw-*` (D13), the fuzz rotation scheduler | `.github/**` | 13 §5 | 4.0 | QA-06 | 2 |
| QA-08 | Provenance and clean-room evidence: trailers, the commit hook, the PR template, `similarity-scan` on an isolated runner, the clean-room checklist | `xtask`, CI | 13 §6 | 3.0 | P0-04 | 1 |
| QA-09 | Codec conformance-suite integration (`suites.toml`, Argon, VP8/VP9 vectors, flac-test-files, PngSuite, JVT/JCT-VC) beyond X-05's fetcher | `vaco-conformance` | 13 §4 | 3.0 | QA-04, X-05 | 4 |
| QA-10 | Release engineering: reproducible-build verification, signing, notarization, SBOM, `release-check` complete, OSS-Fuzz enrolment if §2.6's preconditions hold | `xtask`, CI | 13 §7 | 5.0 | QA-07 | 5 |
| | **Correctness subtotal** | | | **33.5** *(plan states nothing)* | | |

### 1.5 CLI layer (`CL-`, plan 14 §9)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| CL-01 | Option lexer, grouping, descriptors, value grammars | `vaco-cli-core` | 14 §2 | 3.0 | FD-01, FD-06 | 2 |
| CL-02 | Stream-specifier parser + matcher + fuzz target | `vaco-cli-core` | 14 §3 | 1.5 | CL-01 | 2 |
| CL-03 | `vaco-opts` integration: deferred options, `apply_recognised`, the audit pass | `vaco-cli-core` | 14 §2 | 1.0 | CL-01, FD-05 | 2 |
| CL-04 | Help system: `-h`, `-h long/full`, `-h <kind>=<name>`, the listing commands | `vaco-cli-core` | 14 §2 | 1.5 | CL-01, F-04 | 2 |
| CL-05 | Section schema, `TextFormat` façade, `default` + `compact` + `csv` writers | `vaco-textformat` | 14 §4 | 2.0 | FD-01 | 2 |
| CL-06 | `flat`, `ini`, `json`, `xml` writers incl. `q`/`x` modes | `vaco-textformat` | 14 §4 | 2.0 | CL-05 | 2 |
| CL-07 | `num` module: units, prefixes, sexagesimal + the field-table generator | `vaco-textformat` | 14 §4 | 1.5 | CL-05 | 2 |
| CL-08 | `vaco-probe`: option surface + a section emitter for every v0.1 section. **Excludes `-show_frames` per D14.4.** | `vaco-probe` | 14 §5 | 3.0 | CL-06, FM-01..11 | 3 |
| CL-09 | `-show_entries`, `-read_intervals`, `-select_streams` parsers | `vaco-probe` | 14 §5 | 1.0 | CL-08 | 3 |
| CL-10 | Probe acceptance matrix (~9,000 invocations) driven through `vaco-conformance` | `vaco-probe` tests | 14 §5.6 | 1.0 *[−2.0 → XF-01]* | XF-01, QA-02 | 3 |
| CL-11 | `docs/cli/*`, `docs/probe/*` | docs | 14 §11 | 1.0 | CL-09 | 3 |
| CL-12 | `vaco-sched` core: the DAG, wires, node contract, cancellation, EOF ordering. **The hardest single component in the project** (research §05). | `vaco-sched` | 14 §7 | 4.0 | FW-08, F-02, FT-1.3 | 5 |
| CL-13 | Sync queue + interleaving + `-shortest` (packet mode) | `vaco-sched` | 14 §7 | 2.0 | CL-12 | 5 |
| CL-14 | `-map` parser + the stream-selection rules of §6.2 + decision-procedure tests | `vaco` | 14 §6 | 2.5 | CL-02, CL-12 | 5 |
| CL-15 | Timestamp model stages I–III and VI (streamcopy path) | `vaco` | 14 §6 | 3.0 | CL-12, FW-06 | 5 |
| CL-16 | Metadata / disposition / chapter / program mapping options | `vaco` | 14 §6 | 2.0 | CL-14, FW-12 | 5 |
| CL-17 | `-progress`, `-stats`, `-report`, exit codes | `vaco` | 14 §6 | 1.0 | CL-14 | 5 |
| CL-18 | Differential remux tests (container bytes where deterministic) | `vaco` tests | 14 §9 | 2.5 | CL-15, XF-03 | 5 |
| CL-19 | Decoder / encoder nodes, drain semantics, `-frames`, `-pass` | `vaco` | 14 §9 | 3.0 | CL-12, F-02 | 5 |
| CL-20 | Simple filtergraph binding, auto-conversion, `-s`/`-aspect`/`-pix_fmt` placement | `vaco` | 14 §9 | 3.0 | CL-19, FT-2.6 | 5 |
| CL-21 | Timestamp stages IV–V: `-fps_mode`, `-enc_time_base`, `-frame_drop_threshold` | `vaco` | 14 §9 | 3.0 | CL-20 | 5 |
| CL-22 | `-force_key_frames` (all four syntaxes) | `vaco` | 14 §9 | 1.0 | CL-19, FD-06 | 5 |
| CL-23 | `-shortest` frame mode, `-apad`, `-isync` | `vaco` | 14 §9 | 1.5 | CL-21 | 5 |
| CL-24 | The ~600-case timestamp differential matrix | `vaco` tests | 14 §6.4 | 3.0 | CL-23 | 5 |
| CL-25 | `-filter_complex` / `-lavfi`, link-label resolution, unlabeled-pad rules | `vaco` | 14 §9 | 3.0 | CL-20, FT-2.4 | 5 |
| **CL-26** *(group)* | Loopback decoders `[dec:N]`, cycle detection, slack-edge deadlock avoidance — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco`, `vaco-sched` | 14 §9 | **2.0** | CL-25 | 5 |
| CL-26a | `vaco-sched` half: slack edges, cycle detection, deadlock avoidance in the DAG. **Acc:** a synthetic cyclic graph is detected and rejected; a slack-edge graph runs to completion without deadlock under the stress harness | `vaco-sched` | 14 §9 | 1.0 | CL-25 | 5 |
| CL-26b | `vaco` half: `[dec:N]` syntax, loopback decoder wiring and option surface. **Acc:** the loopback-decoder differential cases match the reference's output and exit codes | `vaco` | 14 §9 | 1.0 | CL-26a | 5 |
| CL-27 | `-print_graphs*` + mermaid / mermaidhtml writers | `vaco-textformat` | 14 §9 | 1.5 | CL-25, FT-2.7 | 5 |
| CL-28 | `-stream_group` / IAMF grammar, `-reinit_opts`, `-target` | `vaco` | 14 §9 | 2.5 | CL-25, FM-53 | 5 |
| CL-29 | winit + wgpu presentation path, shader family, present-mode handling | `vaco-play` | 14 §8 | 3.0 | CL-12, SP-A11 | 5 |
| CL-30 | cpal audio out, ring buffer, audio clock from callback timestamps | `vaco-play` | 14 §8 | 2.0 | CL-29, SP-B8 | 5 |
| CL-31 | Clock / sync model, frame dropping, buffering, seek + serials | `vaco-play` | 14 §8 | 2.5 | CL-30 | 5 |
| CL-32 | `waves` / `rdft` display modes | `vaco-play` | 14 §8 | 1.0 | CL-31, SP-C6 | 5 |
| CL-33 | Option surface, the full binding table, stats overlay | `vaco-play` | 14 §8 | 1.5 | CL-31 | 5 |
| **CL-34** *(group)* | Presets, hardware device options, `-analyze_frames`, `-show_log`, device sources/sinks, `-sdp_file`, the remaining expert options — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco`, `vaco-probe` | 14 §9 | **6.0** | CL-28 | 5 |
| CL-34a | `vaco` half: presets, hardware device options, device sources/sinks, `-sdp_file`, the remaining expert options. **Acc:** every option in the group parses and behaves identically to the reference across its differential cases | `vaco` | 14 §9 | 4.0 | CL-28 | 5 |
| CL-34b | `vaco-probe` half: `-analyze_frames`, `-show_log`, the remaining probe expert options. **Acc:** every option's output is byte-identical to the reference across all six writers | `vaco-probe` | 14 §9 | 2.0 | CL-09 | 5 |
| | **CLI subtotal** | | | **75.0** *(plan states 77.0)* | | |

### 1.6 Codec framework and harness (`F-`, `X-`, plan 15 §7.1)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| F-01 | `CodecId` codegen, `CodecParameters`, `Profile`/`Level`, `Caps`, descriptors. **Blocks everything, including `vaco-format-core` per D14.1. Do first, review hard, then freeze.** | `vaco-codec-core` | 15 §7.1 | 3.0 | FD-01, FD-12 | 1 |
| F-02 | `Decoder`/`Encoder`/`Parser`/`BitstreamFilter` traits and the send/receive state machine, with the conformance test every implementation must pass | `vaco-codec-core` | 15 §7.1 | 2.0 | F-01 | 2 |
| F-03 | `ProgressPicture` / `PictureWriter` / `PictureRef` / `PlaneView`. **The highest-risk design item in plan 15.** Land it with a synthetic band-straddle benchmark before any codec depends on it. | `vaco-codec-core` | 15 §1.8 | 4.0 | F-01, FD-11 | 2 |
| F-04 | Registry codegen + feature-model wiring + the `PATENT_ENCUMBERED` CI assertion (D4) | `vaco-registry`, `xtask` | 15 §7.1 | 2.0 | F-01, P0-04 | 1 |
| X-01 | `vaco-conformance` comparator core: byte, framecrc, framemd5, structured-metadata diff | `vaco-conformance` | 15 §7.1 | 4.0 | F-02 | 1 |
| X-02 | ~~`vaco-checkasm`~~ **[dedup −3.0 → PF-0.2]** | `vaco-checkasm` | 15 §7.1 | 0.0 | — | 1 |
| X-03 | Fuzz scaffolding: shared `arbitrary` generators, corpus fetch/minimise, CI wiring | `vaco-fuzz-support` | 15 §7.1 | 2.0 | P0-04 | 1 |
| X-04 | **Quality-based comparison modes** (PSNR/SSIM/VMAF video, spectral metric audio). **On the critical path for every lossy encoder**: without it they are permanently "Unmeasured" and unshippable under D11. | `vaco-conformance` | 15 §7.1 | 3.0 | X-01 | 2 |
| X-05 | `vaco-corpus`: conformance-suite fetching (Argon, VP8/VP9 vectors, flac-test-files, PngSuite, JVT/JCT-VC) | `vaco-corpus` | 15 §7.1 | 2.0 | X-03 | 2 |
| X-06 | D11 CI checks: the single-owner rule for third-party media crates, `cargo-geiger` report, adoption records | `xtask` | 15 §7.1 | 1.0 | F-04 | 1 |
| | **Subtotal** | | | **23.0** | | |

### 1.7 Shared codec DSP, entropy and CBS (`D-`, plan 15 §7.2)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| D-01 | Variable-length code tables and readers | `vaco-codec-vlc` | 15 §7.2 | 3.0 | F-01, FD-03 | 2 |
| D-02 | Exp-Golomb + Rice, read and write | `vaco-codec-golomb` | 15 §7.2 | 2.0 | F-01, FD-03 | 2 |
| D-03 | CABAC engine (engine only) | `vaco-codec-cabac` | 15 §7.2 | 4.0 | F-01 | 2 |
| D-04 | AV1/VP9 multi-symbol + VP8 bool decoder | `vaco-codec-msac` | 15 §7.2 | 3.0 | F-01 | 2 |
| D-05 | Format conversion DSP — **do early, every audio codec needs it** | `vaco-codec-dsp-fmtconvert` | 15 §7.2 | 2.0 | F-01, PF-0.2 | 2 |
| D-06 | Sine window generation | `vaco-codec-dsp-sinewin` | 15 §7.2 | 1.0 | F-01, PF-0.2 | 2 |
| D-07 | LPC analysis and synthesis | `vaco-codec-dsp-lpc` | 15 §7.2 | 3.0 | F-01, PF-0.2 | 2 |
| **D-08** *(group)* | **Generic separable FIR motion compensation, const-generic taps — largest SIMD payoff; start early** — **split into 2 children below; §11.** | `vaco-codec-dsp-mc` | 15 §7.2 | **8.0** | F-01, F-03, PF-0.2, PF-3.2 | 2 |
| D-08a | The const-generic separable FIR engine, tap-set traits, edge emulation and the scalar reference for every tap width, plus the batched-dispatch surface from PF-3.2. **Consumers build against this; D-08b only makes it fast.** **Acc:** every tap width and every sub-pel position bit-exact against a hand-derived reference; the surface is reviewed and frozen. | `vaco-codec-dsp-mc` | 15 §7.2 | 4.0 | F-01, F-03, PF-0.2, PF-3.2 | 2 |
| D-08b | Tier-specific SIMD specialisations for every tap set, the full `vaco-checkasm` differential matrix and the criterion bench suite. **Acc:** 100% of registered MC kernel variants covered by checkasm, bit-identical across `fallback`/SSE2/SSE4.2/AVX2/AVX-512/NEON, none slower than its scalar reference. | `vaco-codec-dsp-mc` | 15 §7.2 | 4.0 | D-08a | 2 |
| D-09 | Intra prediction primitives | `vaco-codec-dsp-intrapred` | 15 §7.2 | 6.0 | F-01, PF-0.2 | 2 |
| D-10 | Deblocking primitives — **gate the design on a measurement spike first** (masked-lane select is the technique) | `vaco-codec-dsp-deblock` | 15 §7.2 | 6.0 | F-01, PF-0.2 | 2 |
| D-11 | IDCT + blockdsp + pixblockdsp | `vaco-codec-dsp-idct` | 15 §7.2 | 5.0 | F-01, PF-0.2 | 2 |
| D-12 | Motion-estimation comparison functions | `vaco-codec-dsp-mecmp` | 15 §7.2 | 4.0 | F-01, PF-0.2 | 4 |
| D-13 | Motion-estimation search patterns | `vaco-codec-dsp-me` | 15 §7.2 | 5.0 | D-12 | 4 |
| D-14 | Rate control | `vaco-codec-dsp-ratecontrol` | 15 §7.2 | 5.0 | F-01, FD-06 | 4 |
| D-15 | Discrete wavelet transform (T2 only) | `vaco-codec-dsp-dwt` | 15 §7.2 | 4.0 | F-01, PF-0.2 | 4 |
| D-16 | ~~`vaco-tx`~~ **[dedup −8.0 → SP-C1..C14; plan 15 under-costed this by 3.4×]** | `vaco-tx` | 15 §7.2 | 0.0 | — | 2 |
| D-17 | Coded-bitstream-syntax core | `vaco-cbs-core` | 15 §7.2 | 3.0 | F-01 | 2 |
| D-18 | H.264/HEVC CBS **read** path | `vaco-cbs-h2645` | 15 §7.2 | 6.0 | D-17 | 2 |
| D-19 | H.264/HEVC CBS **write** path | `vaco-cbs-h2645` | 15 §7.2 | 6.0 | D-18 | 4 |
| D-20 | AV1 CBS | `vaco-cbs-av1` | 15 §7.2 | 4.0 | D-17 | 2 |
| **D-21** *(group)* | VP9 + JPEG CBS — **two independent crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-cbs-vp9`, `vaco-cbs-jpeg` | 15 §7.2 | **4.0** | D-17 | 2 |
| D-21a | VP9 coded-bitstream syntax: uncompressed header, superframe index, fragment read/write. **Acc:** every VP9 test-vector frame round-trips read→write byte-identically | `vaco-cbs-vp9` | 15 §7.2 | 2.0 | D-17 | 2 |
| D-21b | JPEG coded-bitstream syntax: marker segments, scan structure, read/write. **Acc:** every JPEG in the corpus round-trips read→write byte-identically | `vaco-cbs-jpeg` | 15 §7.2 | 2.0 | D-17 | 2 |
| **D-22** *(group)* | **Shared MPEG-family decoder core (H.261/H.263/MPEG-1/2/4, MSMPEG4, WMV1/2, FLV1, RV10/20)** — **split into 4 children below; §11.** | `vaco-codec-mpegvideo` | 15 §7.2 | **14.0** | D-11, D-01, F-03 | 4 |
| D-22a | Picture and MB context model, GOP/picture-type state machine, the per-family hook trait every member implements, and its interface freeze. **Acc:** two stub families compile against the trait and the frozen surface is reviewed; no family-specific code exists above the seam | `vaco-codec-mpegvideo` | 15 §7.2 | 3.0 | F-01, F-03 | 4 |
| D-22b | MB-layer decode loop: block decode, coded-block patterns, MPEG and H.263 dequantisation tables, IDCT integration over D-11. **Acc:** MB-layer output bit-exact against hand-checked vectors for both dequant families at every qscale | `vaco-codec-mpegvideo` | 15 §7.2 | 4.0 | D-22a, D-11, D-01 | 4 |
| D-22c | Shared motion compensation and MV prediction: frame/field, 4MV, OBMC, half-pel, unrestricted MV, wraparound. **Acc:** MC output bit-exact against the scalar reference for every mode combination via `vaco-checkasm` | `vaco-codec-mpegvideo` | 15 §7.2 | 4.0 | D-22b, D-08 | 4 |
| D-22d | Error resilience and resynchronisation (slice/GOB), plus per-family hook validation across all ten members. **Acc:** each of the ten families instantiates the core and decodes a smoke stream; truncation ladder produces no panic and no hang | `vaco-codec-mpegvideo` | 15 §7.2 | 3.0 | D-22c | 4 |
| | **Subtotal** | | | **98.0** | | |

### 1.8 v0.1 parsers (`P-`, plan 15 §7.3)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| P-01 | H.264/HEVC header subset (SPS/PPS/VPS). **Ships in the default build — parsing is not decoding** (plan 15 §5.3). | `vaco-parse-h2645` | 15 §7.3 | 3.0 | D-18 | 2 |
| P-02 | AV1 header subset | `vaco-parse-av1` | 15 §7.3 | 1.0 | D-20 | 2 |
| P-03 | ADTS/LATM/ASC + MP1/2/3 + AC-3 sync | `vaco-parse-mpegaudio` | 15 §7.3 | 3.0 | F-02 | 2 |
| P-04 | Opus / Vorbis / FLAC / ALAC headers | `vaco-parse-audio-misc` | 15 §7.3 | 2.0 | F-02 | 2 |
| P-05 | Profile/level name + constraint tables for H.264, HEVC, AV1, VP9, AAC | `vaco-codec-core` | 15 §7.3 | 2.0 | F-01 | 2 |
| P-06 | VP8/VP9 parser | `vaco-parse-vpx` | 15 §7.3 | 2.0 | D-21 | 2 |
| P-07 | MPEG-1/2/4 video parser | `vaco-parse-mpegvideo` | 15 §7.3 | 3.0 | F-02 | 2 |
| P-08 | Image parsers | `vaco-parse-image` | 15 §7.3 | 3.0 | F-02 | 2 |
| | **Subtotal** | | | **19.0** | | |

### 1.9 T1 codecs (`C-`, plan 15 §7.4)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| C-01 | PCM, table-driven, all 38 decode / 20 encode entries | `vaco-codec-pcm` | 15 §7.4 | 3.0 | D-05 | 4 |
| C-02 | ADPCM, standardised subset (G.722, G.726/le, MS, SWF, IMA-WAV, IMA-QT) | `vaco-codec-adpcm` | 15 §7.4 | 5.0 | D-05 | 4 |
| C-03 | rawvideo, v210, r10k/r210, y41p, avui, bitpacked, wrapped_avframe **+ `vnull`/`anull`** — **C-47 merged in, §11.5** | `vaco-codec-rawvideo`, `vaco-codec-null` | 15 §7.4 | 4.5 | F-01 | 4 |
| C-04 | ass/ssa/srt/webvtt/movtext/text/ttml | `vaco-codec-subtitle-text` | 15 §7.4 | 6.0 | F-01 | 4 |
| C-05 | FLAC **decode via `claxon`** + D11 boundary + fidelity measurement | `vaco-codec-flac` | 15 §7.4 | 3.0 | F-02, X-01 | 4 |
| C-06 | FLAC **native encode** | `vaco-codec-flac` | 15 §7.4 | 6.0 | D-02, D-07 | 4 |
| C-07 | ALAC native decode + encode (`alac` crate as a dev-dependency oracle) | `vaco-codec-alac` | 15 §7.4 | 6.0 | D-02, D-07 | 4 |
| C-08 | PNG **wrapping `png`** + D11 boundary + APNG + colour-metadata mapping | `vaco-codec-png` | 15 §7.4 | 3.0 | F-02, X-01 | 4 |
| C-09 | GIF **wrapping `gif`** + compositing parity | `vaco-codec-gif` | 15 §7.4 | 2.0 | F-02, X-01 | 4 |
| C-10 | TIFF **wrapping `tiff`** + coverage audit | `vaco-codec-tiff` | 15 §7.4 | 2.0 | F-02, X-01 | 4 |
| C-11 | OpenEXR **wrapping `exr`** | `vaco-codec-exr` | 15 §7.4 | 2.0 | F-02, X-01 | 4 |
| C-12 | JPEG XL **wrapping `jxl-oxide`** | `vaco-codec-jpegxl` | 15 §7.4 | 3.0 | F-02, X-01 | 4 |
| **C-13** *(group)* | BMP/PCX/TGA/SGI/XWD/XBM + PNM + QOI, all native — **three independent crates** — **split into 3 children below; §11.** | `vaco-codec-image-simple`, `-pnm`, `-qoi` | 15 §7.4 | **6.0** | F-01 | 4 |
| C-13a | BMP, PCX, TGA, SGI, XWD, XBM decode and encode. **Acc:** every file in the corresponding corpus sections decodes framecrc-identical to the reference; fuzz target green for 24 h | `vaco-codec-image-simple` | 15 §7.4 | 3.0 | F-01 | 4 |
| C-13b | PNM family (pbm/pgm/ppm/pam/pfm/phm) decode and encode. **Acc:** the PNM corpus decodes framecrc-identical and re-encodes byte-identically | `vaco-codec-pnm` | 15 §7.4 | 1.5 | F-01 | 4 |
| C-13c | QOI decode and encode. **Acc:** the QOI reference test images round-trip byte-identically | `vaco-codec-qoi` | 15 §7.4 | 1.5 | F-01 | 4 |
| C-14 | JPEG still decode **wrapping `zune-jpeg`** | `vaco-codec-jpeg` | 15 §7.4 | 2.0 | F-02, X-01 | 4 |
| **C-15** *(group)* | JPEG **native**: the first scheduled D11 native replacement (plan 15 §4A.4) — **split into 3 children below; §11.** | `vaco-codec-jpeg` | 15 §7.4 | **10.0** | D-01, D-11 | 4 |
| C-15a | Baseline sequential DCT decode: marker parsing, frame/scan headers, Huffman tables, MCU loop, restart markers, the spec-exact IDCT mode. **Acc:** the ITU-T T.83 baseline conformance set decodes bit-exact in spec-exact IDCT mode | `vaco-codec-jpeg` | 15 §7.4 | 4.0 | D-01, D-11 | 4 |
| C-15b | Progressive decode, 12-bit precision, all subsampling variants, colour-transform inference (Adobe APP14, JFIF). **Acc:** the T.83 progressive and 12-bit classes decode bit-exact; every subsampling combination in the corpus is covered | `vaco-codec-jpeg` | 15 §7.4 | 3.0 | C-15a | 4 |
| C-15c | MJPEG-A/B framing and container quirks, plus the JPEG encoder (baseline and progressive). **Acc:** MJPEG streams from the AVI/MOV corpus decode framecrc-identical; encoder output re-decodes within the published quality bound | `vaco-codec-jpeg` | 15 §7.4 | 3.0 | C-15b | 4 |
| **C-16** *(group)* | **VP8 decode** — **split into 4 children below; §11.** | `vaco-codec-vp8` | 15 §7.4 | **10.0** | D-04, D-08, D-09, D-10, F-03 | 4 |
| C-16a | Frame header, bool-decoder integration, probability and entropy context model, segmentation, the crate's module map and interface freeze. **Acc:** header field dump matches the reference for every VP8 test vector; probability tables verified against the RFC 6386 reference | `vaco-codec-vp8` | 15 §7.4 | 3.0 | D-04 | 4 |
| C-16b | Intra prediction, DCT and WHT transforms, dequantisation. **Acc:** all-intra vectors decode frame-exact; every prediction mode and transform covered by a `vaco-checkasm` differential | `vaco-codec-vp8` | 15 §7.4 | 3.0 | C-16a, D-09, D-11 | 4 |
| C-16c | Inter prediction: MV decode and prediction, sub-pel MC over D-08, golden/altref reference handling. **Acc:** MV field and MC output match the reference over the inter test vectors | `vaco-codec-vp8` | 15 §7.4 | 2.0 | C-16a, D-08 | 4 |
| C-16d | Loop filter (normal and simple), threading over F-03, conformance against the VP8 test vectors. **Acc:** the full VP8 test-vector set is framemd5-identical to the reference and byte-identical across thread counts | `vaco-codec-vp8` | 15 §7.4 | 2.0 | C-16b, C-16c, D-10, F-03 | 4 |
| **C-17** *(group)* | **VP8 encode** — **split into 4 children below; §11.** | `vaco-codec-vp8` | 15 §7.4 | **12.0** | C-16, D-13, D-14 | 4 |
| C-17a | Encoder skeleton: frame-type decision, bool encoder, probability update, partition packing. **Acc:** encoder output is decodable by C-16 and by the reference decoder for a fixed all-intra input | `vaco-codec-vp8` | 15 §7.4 | 3.0 | C-16d | 4 |
| C-17b | Intra mode decision, quantisation and the RDO cost model. **Acc:** intra-only encode meets the published PSNR/SSIM bound against the reference encoder at matched bitrate | `vaco-codec-vp8` | 15 §7.4 | 3.0 | C-17a | 4 |
| C-17c | Motion estimation integration (D-13), inter mode decision, reference-frame selection. **Acc:** inter encode meets the published quality bound at matched bitrate over the encode corpus | `vaco-codec-vp8` | 15 §7.4 | 3.0 | C-17b, D-13 | 4 |
| C-17d | Rate control (D-14), loop-filter level selection, two-pass, quality gating through X-04. **Acc:** VBR and CBR runs land inside the `quality.lock` bound and the bitrate target tolerance | `vaco-codec-vp8` | 15 §7.4 | 3.0 | C-17c, D-14, X-04 | 4 |
| C-18 | WebP **wrapping `image-webp`** | `vaco-codec-webp` | 15 §7.4 | 2.0 | F-02, X-01 | 4 |
| C-19 | WebP native lossless + route lossy through C-16 | `vaco-codec-webp` | 15 §7.4 | 5.0 | C-16, C-18 | 4 |
| C-20 | Vorbis decode, native, Floor 0 **and** Floor 1 | `vaco-codec-vorbis` | 15 §7.4 | 8.0 | SP-C6, D-06, D-01 | 4 |
| **C-21** *(group)* | **Vorbis encode** — **split into 4 children below; §11.** | `vaco-codec-vorbis` | 15 §7.4 | **12.0** | C-20 | 4 |
| C-21a | Codebook construction and header/setup packet generation. **Acc:** generated headers are accepted by C-20 and by the reference decoder; codebooks round-trip | `vaco-codec-vorbis` | 15 §7.4 | 3.0 | C-20 | 4 |
| C-21b | Psychoacoustic model and floor-1 curve fitting. **Acc:** floor curves reproduce the reference's spectral envelope within the published tolerance on the encode corpus | `vaco-codec-vorbis` | 15 §7.4 | 4.0 | C-21a | 4 |
| C-21c | Residue encoding (all three types), channel coupling, quantisation. **Acc:** encoded residues decode back within the published quality bound at matched bitrate | `vaco-codec-vorbis` | 15 §7.4 | 3.0 | C-21b | 4 |
| C-21d | Mode and blocksize decision, bitrate management, quality gating through X-04. **Acc:** full encodes land inside `quality.lock` across the corpus at three quality settings | `vaco-codec-vorbis` | 15 §7.4 | 2.0 | C-21c, X-04 | 4 |
| C-22 | Opus range decoder + packet framing | `vaco-codec-opus` | 15 §7.4 | 2.0 | F-02 | 4 |
| C-23 | Opus CELT decode | `vaco-codec-opus` | 15 §7.4 | 5.0 | C-22, SP-C6, D-06 | 4 |
| C-24 | Opus SILK decode | `vaco-codec-opus` | 15 §7.4 | 5.0 | C-22, D-07 | 4 |
| C-25 | Opus hybrid, multistream, PLC/FEC, integration | `vaco-codec-opus` | 15 §7.4 | 4.0 | C-23, C-24 | 4 |
| **C-26** *(group)* | **Opus encoder** — **split into 6 children below; §11.** | `vaco-codec-opus` | 15 §7.4 | **20.0** | C-25, X-04 | 4 |
| C-26a | Encoder framing: mode/bandwidth/frame-size decision, range encoder, packet assembly (all four TOC configurations). **Acc:** every produced packet is decodable by C-25 and by the reference decoder; TOC round-trips | `vaco-codec-opus` | 15 §7.4 | 3.0 | C-25 | 4 |
| C-26b | CELT encode: MDCT analysis over SP-C6, band energy quantisation, PVQ search. **Acc:** CELT-only encodes decode within the published quality bound at matched bitrate | `vaco-codec-opus` | 15 §7.4 | 5.0 | C-26a, SP-C6 | 4 |
| C-26c | CELT bit allocation, spreading and folding, transient detection, prefilter/postfilter. **Acc:** allocation matches the reference's bit budget within tolerance across the corpus; transient cases verified explicitly | `vaco-codec-opus` | 15 §7.4 | 4.0 | C-26b | 4 |
| C-26d | SILK encode: LPC and LTP analysis over D-07, noise-shaping quantiser. **Acc:** SILK-only encodes decode within the published quality bound at matched bitrate | `vaco-codec-opus` | 15 §7.4 | 5.0 | C-26a, D-07 | 4 |
| C-26e | Hybrid mode, DTX, CBR/VBR/CVBR rate control, FEC/LBRR. **Acc:** hybrid and FEC encodes decode correctly under simulated 5% and 20% packet loss | `vaco-codec-opus` | 15 §7.4 | 2.0 | C-26c, C-26d | 4 |
| C-26f | Quality gating through X-04, RFC 6716 encoder-side vectors, multistream and surround encode. **Acc:** `quality.lock` populated and ratcheted for every mode; the RFC's encoder conditions are met | `vaco-codec-opus` | 15 §7.4 | 1.0 | C-26e, X-04 | 4 |
| C-27 | FFV1 decode | `vaco-codec-ffv1` | 15 §7.4 | 8.0 | D-02 | 4 |
| C-28 | FFV1 encode | `vaco-codec-ffv1` | 15 §7.4 | 6.0 | C-27 | 4 |
| C-29 | VP9 headers, superframes, bool decoder, probability model | `vaco-codec-vp9` | 15 §7.4 | 5.0 | D-04, D-21 | 4 |
| C-30 | VP9 intra + transforms | `vaco-codec-vp9` | 15 §7.4 | 7.0 | C-29, D-09 | 4 |
| C-31 | VP9 inter + MV prediction | `vaco-codec-vp9` | 15 §7.4 | 5.0 | C-29, D-08 | 4 |
| **C-32** *(group)* | **VP9 loop filter + profiles 1–3 + threading + conformance** — **split into 3 children below; §11.** | `vaco-codec-vp9` | 15 §7.4 | **9.0** | C-30, C-31, D-10, F-03 | 4 |
| C-32a | Loop filter: level and sharpness derivation, all filter widths, segment and reference deltas. **Acc:** loop-filter output bit-exact against the spec reference over a synthetic edge-exhaustive corpus | `vaco-codec-vp9` | 15 §7.4 | 3.0 | C-30, C-31, D-10 | 4 |
| C-32b | Profiles 1–3: 4:2:2, 4:4:4 and 4:4:0 chroma, 10- and 12-bit paths. **Acc:** the profile-1/2/3 test vectors decode framemd5-identical to the reference | `vaco-codec-vp9` | 15 §7.4 | 3.0 | C-30, C-31 | 4 |
| C-32c | Tile and frame threading over F-03, DPB and reference-frame scaling, conformance bring-up. **Acc:** the full VP9 test-vector set is framemd5-identical and byte-identical across thread counts | `vaco-codec-vp9` | 15 §7.4 | 3.0 | C-32a, C-32b, F-03 | 4 |
| **C-33** *(group)* | **VP9 encode** — **split into 6 children below; §11.** | `vaco-codec-vp9` | 15 §7.4 | **22.0** | C-32, D-13, D-14, X-04 | 4 |
| C-33a | Encoder skeleton: frame and superframe assembly, entropy encode, probability/CDF update, the encoder's internal interface freeze. **Acc:** output is decodable by C-32 and by the reference decoder for a fixed all-intra input | `vaco-codec-vp9` | 15 §7.4 | 4.0 | C-32c | 4 |
| C-33b | Partition search and intra mode decision. **Acc:** intra-only encode meets the published quality bound at matched bitrate | `vaco-codec-vp9` | 15 §7.4 | 4.0 | C-33a | 4 |
| C-33c | Inter mode decision, MV search integration (D-13), compound and reference-frame selection. **Acc:** inter encode meets the published quality bound at matched bitrate over the encode corpus | `vaco-codec-vp9` | 15 §7.4 | 5.0 | C-33b, D-13 | 4 |
| C-33d | Transform type and size selection, quantisation, the RDO cost model. **Acc:** the rate-distortion curve is within the published bound of the reference encoder at three speed settings | `vaco-codec-vp9` | 15 §7.4 | 4.0 | C-33b | 4 |
| C-33e | Rate control (D-14), speed-feature tiers, two-pass encode. **Acc:** CBR/VBR/two-pass runs hit the bitrate target inside tolerance and inside `quality.lock` | `vaco-codec-vp9` | 15 §7.4 | 3.0 | C-33c, C-33d, D-14 | 4 |
| C-33f | Quality gating through X-04 and the decode round-trip matrix. **Acc:** every encode in the corpus round-trips through C-32 and the reference decoder identically | `vaco-codec-vp9` | 15 §7.4 | 2.0 | C-33e, X-04 | 4 |
| C-34 | AV1 OBU layer, sequence header, `av1C`, Annex-B | `vaco-codec-av1` | 15 §7.4 | 5.0 | D-20 | 4 |
| C-35 | AV1 frame header, reference management, tile info | `vaco-codec-av1` | 15 §7.4 | 8.0 | C-34 | 4 |
| C-36 | AV1 symbol decoder + CDF machinery | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-34, D-04 | 4 |
| C-37 | AV1 tile/superblock loop, partition tree, mode info | `vaco-codec-av1` | 15 §7.4 | 5.0 | C-35, C-36 | 4 |
| C-38 | AV1 intra prediction incl. CFL, palette, intrabc | `vaco-codec-av1` | 15 §7.4 | 8.0 | C-37, D-09 | 4 |
| **C-39** *(group)* | **AV1 inter prediction** — **split into 3 children below; §11.** | `vaco-codec-av1` | 15 §7.4 | **12.0** | C-37, D-08 | 4 |
| C-39a | MV stack: reference-MV candidate scan, dynamic reference list, MV projection and scaling, `ref_mv_idx` semantics. **Acc:** candidate stacks match a hand-derived reference over the Argon inter-prediction profiles | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-37 | 4 |
| C-39b | Sub-pel MC filters over D-08, compound prediction, wedge/difference-weighted/inter-intra masked modes. **Acc:** every interpolation and mask combination bit-exact against the scalar reference via `vaco-checkasm` | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-37, D-08 | 4 |
| C-39c | Warped motion (global and local warp), OBMC, motion-field estimation. **Acc:** the Argon warp and OBMC profiles decode frame-exact | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-37, C-39a | 4 |
| C-40 | AV1 transforms | `vaco-codec-av1` | 15 §7.4 | 8.0 | C-37 | 4 |
| **C-41** *(group)* | **AV1 deblocking, CDEF, superres, loop restoration** — **split into 4 children below; §11.** Four independent post-filters that were sitting on the critical path as one 8 pw block. | `vaco-codec-av1` | 15 §7.4 | **8.0** | C-37, D-10 | 4 |
| C-41a | AV1 deblocking: level and limit derivation, edge filtering, per-plane and per-segment deltas. **Acc:** deblocking output bit-exact against the spec reference over a synthetic edge-exhaustive corpus. | `vaco-codec-av1` | 15 §7.4 | 2.0 | C-37, D-10 | 4 |
| C-41b | CDEF: direction search, primary and secondary filtering, damping, skip handling. **Acc:** the Argon CDEF profiles decode frame-exact; every direction/strength pair covered by a `vaco-checkasm` differential. | `vaco-codec-av1` | 15 §7.4 | 2.0 | C-37 | 4 |
| C-41c | Superres: the linear upscaler, coefficient tables, frame-size-with-refs interaction. **Acc:** the Argon superres profiles decode frame-exact. | `vaco-codec-av1` | 15 §7.4 | 2.0 | C-37 | 4 |
| C-41d | Loop restoration: Wiener and self-guided filters, unit and stripe geometry, boundary handling. **Acc:** the Argon loop-restoration profiles decode frame-exact. | `vaco-codec-av1` | 15 §7.4 | 2.0 | C-41b, C-41c | 4 |
| C-42 | AV1 film grain | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-41a, C-41d | 4 |
| C-43 | AV1 tile + frame threading, DPB, integration | `vaco-codec-av1` | 15 §7.4 | 5.0 | C-38..C-42, F-03 | 4 |
| C-44 | AV1 Argon conformance bring-up and triage | `vaco-codec-av1` | 15 §7.4 | 3.0 | C-43, X-05, QA-09 | 4 |
| C-45 | AV1 **encode wrapping `rav1e`** (`default-features=false`, no asm) + D11 boundary + quality baselines | `vaco-codec-av1` | 15 §7.4 | 4.0 | C-34, X-04 | 4 |
| C-46 | Spawn a user-installed encoder. **Solves x264/x265 and gives T5 an escape hatch — schedule early, it is 4 pw that answers the project's most common question.** | `vaco-codec-exec` | 15 §7.4 | 4.0 | F-02 | 4 |
| C-47 | ~~`vnull` / `anull`~~ **[merged → C-03, §11.5]** | `vaco-codec-null` | 15 §7.4 | 0.0 | — | 4 |
| | **Subtotal** | | | **288.5** | | |

### 1.10 Bitstream filters, T2 codecs, hardware (`B-`, `T2-`, `H-`, plan 15 §7.5)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| B-01 | BSF core + generic filters | `vaco-bsf-core`, `-generic` | 15 §7.5 | 5.0 | F-02, D-17 | 4 |
| B-02 | H.264/HEVC BSFs (`*_mp4toannexb` first) | `vaco-bsf-h2645` | 15 §7.5 | 4.0 | D-18 | 4 |
| B-03 | AV1 + VPx BSFs | `vaco-bsf-av1`, `-vpx` | 15 §7.5 | 3.0 | D-20, D-21 | 4 |
| B-04 | Audio BSFs (`aac_adtstoasc` first) | `vaco-bsf-audio` | 15 §7.5 | 3.0 | P-03 | 4 |
| B-05 | `*_metadata` filters (needs the CBS write path) | `vaco-bsf-*` | 15 §7.5 | 4.0 | D-19 | 4 |
| B-06 | Subtitle + legacy BSFs | `vaco-bsf-subtitle`, `-legacy` | 15 §7.5 | 4.0 | B-01 | 4 |
| **T2-01** *(group)* | **MPEG-1/2 video decode + encode** — **split into 3 children below; §11.** | `vaco-codec-mpeg12` | 15 §7.5 | **12.0** | D-22 | 4 |
| T2-01a | MPEG-1/2 decode: sequence/GOP/picture headers and extensions, MB layer over D-22, field and frame pictures, pulldown flags. **Acc:** the MPEG-2 conformance streams decode framemd5-identical to the reference | `vaco-codec-mpeg12` | 15 §7.5 | 5.0 | D-22d | 4 |
| T2-01b | MPEG-2 extensions: 4:2:2 profile, intra-VLC table, alternate scan, all chroma formats, conformance bring-up. **Acc:** the 4:2:2-profile conformance streams decode framemd5-identical; every scan/VLC combination exercised | `vaco-codec-mpeg12` | 15 §7.5 | 3.0 | T2-01a | 4 |
| T2-01c | MPEG-1/2 encode: mode decision, quantiser matrices, rate control, GOP structure, closed/open GOP. **Acc:** encoded streams decode identically under both our decoder and the reference, and land inside `quality.lock` | `vaco-codec-mpeg12` | 15 §7.5 | 4.0 | T2-01b, D-14, X-04 | 4 |
| **T2-02** *(group)* | MPEG-4 Part 2, H.263, H.261 — **two crates, split for single-writer ownership** — **split into 4 children below; §11.** | `vaco-codec-mpeg4`, `-h263` | 15 §7.5 | **12.0** | D-22 | 4 |
| T2-02a | H.261 and baseline H.263 decode. **Acc:** the H.261/H.263 conformance streams decode framemd5-identical to the reference | `vaco-codec-h263` | 15 §7.5 | 3.0 | D-22d | 4 |
| T2-02b | H.263+ annexes (D/E/F/G/I/J/K/L/P/T) plus the RV10/20 and FLV1 family hooks. **Acc:** each annex has a decoding test; RV10/20 and FLV1 smoke streams decode framemd5-identical | `vaco-codec-h263` | 15 §7.5 | 3.0 | T2-02a | 4 |
| T2-02c | MPEG-4 Part 2 decode: VOL/VOP headers, short header, quarter-pel MC, GMC, interlaced, data partitioning. **Acc:** the MPEG-4 Part 2 conformance streams decode framemd5-identical, including GMC and interlaced cases | `vaco-codec-mpeg4` | 15 §7.5 | 4.0 | D-22d | 4 |
| T2-02d | MPEG-4 Part 2 and H.263 encode paths. **Acc:** encoded streams decode identically under our decoder and the reference and land inside `quality.lock` | `vaco-codec-mpeg4` | 15 §7.5 | 2.0 | T2-02c, T2-02b, X-04 | 4 |
| **T2-03** *(group)* | **MP1/MP2/MP3 decode (+ MP2/MP3 encode)** — **split into 5 children below; §11.** | `vaco-codec-mpegaudio` | 15 §7.5 | **14.0** | SP-C6, D-01 | 4 |
| T2-03a | Layer I and II decode: header sync, bit allocation, scalefactors, synthesis filterbank over SP-C6. **Acc:** the ISO 11172-4 Layer I/II vectors decode inside the specification's RMS tolerance | `vaco-codec-mpegaudio` | 15 §7.5 | 4.0 | SP-C6, D-01 | 4 |
| T2-03b | Layer III decode: side info, Huffman decode, requantisation, stereo modes, alias reduction, IMDCT, hybrid synthesis. **Acc:** the ISO 11172-4 Layer III vectors decode inside the specification's RMS tolerance and bit-exactly under the fixed-point contract | `vaco-codec-mpegaudio` | 15 §7.5 | 5.0 | T2-03a | 4 |
| T2-03c | Free-format streams, MPEG-2/2.5 low sample-rate extension, gapless metadata (LAME/Xing/VBRI), conformance bring-up. **Acc:** the purchased conformance suite passes; gapless trimming matches the reference sample-for-sample | `vaco-codec-mpegaudio` | 15 §7.5 | 2.0 | T2-03b, QA-09 | 4 |
| T2-03d | MP2 encode: psychoacoustic model I, bit allocation. **Acc:** encoded streams decode identically under our decoder and the reference and land inside `quality.lock` | `vaco-codec-mpegaudio` | 15 §7.5 | 2.0 | T2-03a, X-04 | 4 |
| T2-03e | MP3 encode: psychoacoustic model II, the quantisation loop, bit reservoir. **Acc:** encoded streams land inside `quality.lock` at three bitrates and decode identically under both decoders | `vaco-codec-mpegaudio` | 15 §7.5 | 1.0 | T2-03d, T2-03b | 4 |
| **T2-04** *(group)* | AC-3 decode. **E-AC-3 is gated on the D9 expiry verification — do not ship until counsel confirms** — **split into 4 children below; §11.** | `vaco-codec-ac3` | 15 §7.5 | **12.0** | SP-C6, D-06 | 4 |
| T2-04a | AC-3 bitstream: sync frame, BSI, exponent strategies and decode, bit-allocation model. **Acc:** the ATSC A/52 conformance vectors produce bit-exact exponent and allocation arrays | `vaco-codec-ac3` | 15 §7.5 | 4.0 | SP-C6, D-06 | 4 |
| T2-04b | AC-3 reconstruction: mantissa dequantisation, coupling, rematrixing, IMDCT/windowing over SP-C6, downmix, dynamic range. **Acc:** the A/52 vectors decode inside the specification's tolerance at every channel configuration | `vaco-codec-ac3` | 15 §7.5 | 4.0 | T2-04a | 4 |
| T2-04c | E-AC-3 extensions: AHT, spectral extension, enhanced coupling, dependent substreams. **Blocked on the D9 expiry verification**. **Acc:** the E-AC-3 vectors decode inside tolerance **and** counsel's written confirmation is recorded before the feature is enabled | `vaco-codec-ac3` | 15 §7.5 | 3.0 | T2-04b | 4 |
| T2-04d | Conformance matrix: dialnorm, DRC profiles and downmix differential across all channel configurations. **Acc:** every dialnorm/DRC/downmix combination matches the reference within tolerance | `vaco-codec-ac3` | 15 §7.5 | 1.0 | T2-04c, QA-09 | 4 |
| T2-05 | Theora decode | `vaco-codec-theora` | 15 §7.5 | 8.0 | D-11, D-01 | 4 |
| T2-06 | DV decode + encode | `vaco-codec-dv` | 15 §7.5 | 8.0 | D-11, D-01 | 4 |
| **T2-07** *(group)* | **JPEG 2000 decode + encode** — **split into 4 children below; §11.** | `vaco-codec-jpeg2000` | 15 §7.5 | **16.0** | D-15 | 4 |
| T2-07a | Codestream and JP2 container syntax: markers, tiles, components, COD/QCD/QCC, packet headers, progression parsing. **Acc:** every file in the T.803 corpus parses to the same structural description the reference reports | `vaco-codec-jpeg2000` | 15 §7.5 | 4.0 | D-15 | 4 |
| T2-07b | EBCOT Tier-1: the MQ arithmetic coder, the three coding passes, context modelling. **Acc:** code-block decode is bit-exact against hand-checked vectors for every context and pass combination | `vaco-codec-jpeg2000` | 15 §7.5 | 5.0 | T2-07a | 4 |
| T2-07c | Tier-2, precincts, all progression orders, DWT integration over D-15, dequantisation, decode integration. **Acc:** the T.803 decoder conformance classes decode bit-exact for reversible and within tolerance for irreversible paths | `vaco-codec-jpeg2000` | 15 §7.5 | 4.0 | T2-07b | 4 |
| T2-07d | Encode path with rate allocation, plus encoder conformance. **Acc:** encoded codestreams decode identically under our decoder and the reference and land inside `quality.lock` | `vaco-codec-jpeg2000` | 15 §7.5 | 3.0 | T2-07c, X-04 | 4 |
| T2-08 | JPEG-LS decode + encode | `vaco-codec-jpegls` | 15 §7.5 | 5.0 | D-02 | 4 |
| **T2-09** *(group)* | ProRes decode / DNxHD decode (decode-only per the legal register) — **two crates, split for single-writer ownership** — **split into 4 children below; §11.** | `vaco-codec-prores`, `-dnxhd` | 15 §7.5 | **14.0** | D-11 | 4 |
| T2-09a | ProRes decode: frame and picture headers, slice layout, entropy decode, DCT over D-11, profiles Proxy/LT/422/422HQ. **Acc:** the ProRes corpus decodes framemd5-identical to the reference at all four profiles | `vaco-codec-prores` | 15 §7.5 | 5.0 | D-11 | 4 |
| T2-09b | ProRes 4444 and 4444 XQ: alpha channel, 12-bit paths, conformance bring-up. **Acc:** the 4444 corpus decodes framemd5-identical including alpha | `vaco-codec-prores` | 15 §7.5 | 2.0 | T2-09a | 4 |
| T2-09c | DNxHD/VC-3 decode: CID table, MB layer, DCT, 8- and 10-bit paths. **Acc:** the DNxHD corpus decodes framemd5-identical across every CID in the table | `vaco-codec-dnxhd` | 15 §7.5 | 5.0 | D-11 | 4 |
| T2-09d | DNxHR variants (LB/SQ/HQ/HQX/444), alpha, conformance bring-up. **Acc:** the DNxHR corpus decodes framemd5-identical at every variant | `vaco-codec-dnxhd` | 15 §7.5 | 2.0 | T2-09c | 4 |
| **T2-10** *(group)* | **VC-1 / WMV3 decode** — **split into 5 children below; §11.** | `vaco-codec-vc1` | 15 §7.5 | **16.0** | D-22, D-08, D-10 | 4 |
| T2-10a | Sequence, entry-point and picture headers, BDU parsing, simple/main/advanced profile handling. **Acc:** header field dump matches the reference for the SMPTE 421M conformance streams at all three profiles | `vaco-codec-vc1` | 15 §7.5 | 3.0 | D-22d | 4 |
| T2-10b | Entropy decode and MB layer: VLC tables, bitplane coding (all seven modes), CBP, transform-type signalling. **Acc:** bitplane and CBP decode is bit-exact against hand-checked vectors for every coding mode | `vaco-codec-vc1` | 15 §7.5 | 4.0 | T2-10a | 4 |
| T2-10c | Intra prediction, AC/DC prediction, the four integer transforms, dequantisation. **Acc:** all-intra conformance streams decode frame-exact | `vaco-codec-vc1` | 15 §7.5 | 3.0 | T2-10b | 4 |
| T2-10d | Inter prediction: MV prediction, quarter-pel and bilinear MC over D-08, intensity compensation, interlaced field and frame modes. **Acc:** the inter and interlaced conformance streams decode framemd5-identical | `vaco-codec-vc1` | 15 §7.5 | 4.0 | T2-10b, D-08 | 4 |
| T2-10e | Overlap smoothing, the in-loop deblocking filter over D-10, range reduction/mapping, conformance bring-up. **Acc:** the full SMPTE 421M conformance set decodes framemd5-identical with every failure enumerated | `vaco-codec-vc1` | 15 §7.5 | 2.0 | T2-10c, T2-10d, D-10 | 4 |
| **T2-11** *(group)* | **Dirac / VC-2** — **split into 4 children below; §11.** | `vaco-codec-dirac` | 15 §7.5 | **12.0** | D-15 | 4 |
| T2-11a | Stream syntax: parse info headers, sequence and picture headers, VC-2 low-delay and HQ profiles. **Acc:** every stream in the VC-2 corpus parses to the same structural description the reference reports | `vaco-codec-dirac` | 15 §7.5 | 3.0 | D-15 | 4 |
| T2-11b | Wavelet transform integration over D-15 (all filter kinds), slice and coefficient decode, dequantisation. **Acc:** intra-only VC-2 streams decode bit-exact against the specification's reference transform | `vaco-codec-dirac` | 15 §7.5 | 4.0 | T2-11a | 4 |
| T2-11c | Dirac arithmetic coding, OBMC motion compensation, inter pictures. **Acc:** the Dirac inter corpus decodes framemd5-identical to the reference | `vaco-codec-dirac` | 15 §7.5 | 3.0 | T2-11b | 4 |
| T2-11d | VC-2 encode and conformance bring-up. **Acc:** encoded streams decode identically under our decoder and the reference; the VC-2 conformance set passes | `vaco-codec-dirac` | 15 §7.5 | 2.0 | T2-11c, X-04 | 4 |
| T2-12 | G.711/722/726/729 + SBC + comfort noise + dfpwm + QOA | `vaco-codec-speech` | 15 §7.5 | 8.0 | D-05 | 4 |
| **T2-13** *(group)* | Bitmap and text subtitle decoders (DVB, DVD, PGS, CEA-608/708, Teletext) — **split by format family across three crates** — **split into 5 children below; §11.** | `vaco-codec-subtitle-bitmap`, `-cc`, `-teletext` | 15 §7.5 | **14.0** | F-02 | 4 |
| T2-13a | DVD (`vobsub`) and DVB subtitle decode: region/CLUT/object model, RLE decode, display sets. **Acc:** the subtitle corpus renders bitmap-identical to the reference for both formats | `vaco-codec-subtitle-bitmap` | 15 §7.5 | 4.0 | F-02 | 4 |
| T2-13b | PGS / HDMV presentation graphics decode: composition, window and palette segments, object decode. **Acc:** the PGS corpus renders bitmap-identical to the reference including forced-subtitle handling | `vaco-codec-subtitle-bitmap` | 15 §7.5 | 3.0 | T2-13a | 4 |
| T2-13c | CEA-608 decode: field and channel parsing, pop-on/roll-up/paint-on modes, styling and positioning. **Acc:** the CEA-608 corpus produces text and timing identical to the reference | `vaco-codec-subtitle-cc` | 15 §7.5 | 3.0 | F-02 | 4 |
| T2-13d | CEA-708 decode: DTVCC packet assembly, service blocks, window and pen commands. **Acc:** the CEA-708 corpus produces text, timing and window geometry identical to the reference | `vaco-codec-subtitle-cc` | 15 §7.5 | 2.0 | T2-13c | 4 |
| T2-13e | Teletext decode (EBU 300 706) including level 1.5 presentation. **Acc:** the Teletext corpus produces pages identical to the reference | `vaco-codec-subtitle-teletext` | 15 §7.5 | 2.0 | F-02 | 4 |
| **T2-14** *(group)* | APV, JPEG XS (assess crates first per D10) — **two crates, split for single-writer ownership** — **split into 3 children below; §11.** | `vaco-codec-apv`, `-jpegxs` | 15 §7.5 | **10.0** | F-02 | 4 |
| T2-14a | APV decode: frame and tile syntax, entropy decode, transform and reconstruction. **Acc:** the APV reference streams decode framemd5-identical | `vaco-codec-apv` | 15 §7.5 | 4.0 | F-02 | 4 |
| T2-14b | APV encode and conformance bring-up. **Acc:** encoded streams decode identically under our decoder and the reference and land inside `quality.lock` | `vaco-codec-apv` | 15 §7.5 | 2.0 | T2-14a, X-04 | 4 |
| T2-14c | JPEG XS decode: codestream syntax, wavelet, entropy decode, all profiles in scope. **Acc:** the JPEG XS reference streams decode framemd5-identical | `vaco-codec-jpegxs` | 15 §7.5 | 4.0 | F-02 | 4 |
| H-01 | Device/frames contexts, `HwAccel` trait, `Frame` hw storage, selection and fallback. **D13 raises this from peripheral to strategic — it is how H.264/HEVC reach users at all.** | `vaco-hw-core` | 15 §7.5, D13 | 8.0 | F-02, FD-11 | 3 |
| **H-02** *(group)* | **VideoToolbox decode + encode (`objc2-video-toolbox`)** — **split into 3 children below; §11.** | `vaco-hw-videotoolbox` | 15 §7.5, D13 | **11.0** | H-01 | 3 |
| H-02a | Session and format-description layer: `objc2` binding surface, format descriptions, pixel-buffer ↔ `Frame` mapping, device/frames-context integration. **Acc:** a pixel buffer round-trips to a `Frame` and back with no copy and no leak under the sanitizer job (D13) | `vaco-hw-videotoolbox` | 15 §7.5, D13 | 4.0 | H-01 | 3 |
| H-02b | Decode path: H.264/HEVC/ProRes/AV1 decode sessions, asynchronous output reordering, DPB interaction, fallback on session failure. **Acc:** hardware decode output is differentially identical to the software decoder for the supported codecs (D13 verification item 1) | `vaco-hw-videotoolbox` | 15 §7.5, D13 | 4.0 | H-02a | 3 |
| H-02c | Encode path: H.264/HEVC encode sessions, rate control, the property/option surface. **Acc:** encoded streams decode correctly under the reference decoder and land inside `quality.lock` | `vaco-hw-videotoolbox` | 15 §7.5, D13 | 3.0 | H-02a | 3 |
| **H-06** *(group)* | **Vulkan Video** decode + encode (`ash`). **D13's "best single investment"** — **split into 3 children below; §11.** | `vaco-hw-vulkan-video` | 15 §7.5, D13 | **12.0** | H-01 | 3 |
| H-06a | Instance/device/queue bring-up via `ash`, video-capability query, memory allocation and image-layout handling, device/frames-context integration. **Acc:** the capability query reports the expected profile set on at least two vendors and images round-trip to `Frame` without a copy | `vaco-hw-vulkan-video` | 15 §7.5, D13 | 4.0 | H-01 | 3 |
| H-06b | Decode: video-session and session-parameters objects, H.264/HEVC/AV1 decode profiles, DPB slot management. **Acc:** hardware decode output is differentially identical to the software decoder for the supported codecs (D13 verification item 1) | `vaco-hw-vulkan-video` | 15 §7.5, D13 | 5.0 | H-06a | 3 |
| H-06c | Encode: H.264/HEVC encode sessions, rate control, quality levels. **Acc:** encoded streams decode correctly under the reference decoder and land inside `quality.lock` | `vaco-hw-vulkan-video` | 15 §7.5, D13 | 3.0 | H-06a | 3 |
| **H-03** *(group)* | VA-API — **conditional.** D13: "only if Vulkan Video proves insufficient in practice" — **split into 2 children below; §11.** | `vaco-hw-vaapi` | 15 §7.5 | **11.0 *(conditional)*** | H-01, H-06 | 4 |
| H-03a | Device/surface layer and decode path. **Acc:** hardware decode output is differentially identical to the software decoder for the supported codecs | `vaco-hw-vaapi` | 15 §7.5 | 7.0 *(conditional)* | H-01, H-06b | 4 |
| H-03b | Encode path and option surface. **Acc:** encoded streams decode correctly under the reference decoder and land inside `quality.lock` | `vaco-hw-vaapi` | 15 §7.5 | 4.0 *(conditional)* | H-03a | 4 |
| **H-04** *(group)* | D3D12 Video — **conditional.** D13: "Vulkan Video already covers Windows" — **split into 2 children below; §11.** | `vaco-hw-d3d12` | 15 §7.5, D13 | **11.0 *(conditional)*** | H-01, H-06 | 4 |
| H-04a | Device/resource layer and decode path. **Acc:** hardware decode output is differentially identical to the software decoder for the supported codecs | `vaco-hw-d3d12` | 15 §7.5, D13 | 7.0 *(conditional)* | H-01, H-06b | 4 |
| H-04b | Encode path and option surface. **Acc:** encoded streams decode correctly under the reference decoder and land inside `quality.lock` | `vaco-hw-d3d12` | 15 §7.5, D13 | 4.0 *(conditional)* | H-04a | 4 |
| **H-05** *(group)* | NVDEC / NVENC — **conditional**, same test as H-03 — **split into 2 children below; §11.** | `vaco-hw-nvdec` | 15 §7.5 | **11.0 *(conditional)*** | H-01, H-06 | 4 |
| H-05a | Device/context layer and NVDEC decode path. **Acc:** hardware decode output is differentially identical to the software decoder for the supported codecs | `vaco-hw-nvdec` | 15 §7.5 | 7.0 *(conditional)* | H-01, H-06b | 4 |
| H-05b | NVENC encode path and option surface. **Acc:** encoded streams decode correctly under the reference decoder and land inside `quality.lock` | `vaco-hw-nvdec` | 15 §7.5 | 4.0 *(conditional)* | H-05a | 4 |
| H-07 | Hardware conformance matrix + fallback tests in CI, differentially gated against the in-tree software decoder (D13 §Verification item 1) | CI | 15 §7.5 | 4.0 | H-02, H-06, T3-01 | 4 |
| | **Subtotal (unconditional)** | | | **219.0** | | |
| | **Subtotal (conditional H-03/04/05)** | | | **+33.0** | | |

### 1.11 T3 and beyond (`T3-`, `T4-`, `T5-`, plan 15 §7.6)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| **T3-01** *(group)* | H.264 decode, `patent-encumbered-h264-decode`. **Never shipped.** Validates the hwaccel parse-half and serves licensed users — **split into 14 children below; §11.** | `vaco-codec-h264` | 15 §7.6 | **60.0** | D-03, D-08, D-09, D-10, D-18, F-03 | 4 |
| T3-01a | NAL/bitstream layer: Annex-B and length-prefixed input, RBSP unescaping, NAL header semantics, access-unit boundary detection, the crate's module map and internal interface freeze. **Acc:** every AU in the JVT base corpus is split identically to the reference's `-c copy` packet boundaries; fuzz target on NAL scanning | `vaco-codec-h264` | 15 §7.6 | 4.0 | D-18, F-02 | 4 |
| T3-01b | Parameter sets: SPS, PPS, SPS-extension, subset SPS, VUI, HRD, scaling lists, profile/level constraint derivation (reuses P-01's parse-only subset). **Acc:** every SPS/PPS in the JVT corpus round-trips to the same field values P-01 reports, and unsupported combinations are rejected rather than mis-decoded | `vaco-codec-h264` | 15 §7.6 | 3.0 | T3-01a, P-01 | 4 |
| T3-01c | Slice header parsing (§7.3.3): all slice types, `first_mb_in_slice`, weighted-pred tables, `dec_ref_pic_marking` syntax, slice-group/ASO maps. **Acc:** slice-header field dump matches the reference for every stream in the base corpus, including FMO/ASO streams | `vaco-codec-h264` | 15 §7.6 | 3.0 | T3-01b | 4 |
| T3-01d | CAVLC entropy decoding (§9.2): `coeff_token`, level prefix/suffix, `total_zeros`, `run_before`, and the residual block layer. **Acc:** the CAVLC-only subset of the JVT baseline conformance set decodes to bit-exact residual coefficient arrays against hand-checked vectors | `vaco-codec-h264` | 15 §7.6 | 5.0 | T3-01c, D-01 | 4 |
| T3-01e | CABAC binarisation and context modelling for H.264 (§9.3) on top of D-03's engine: context index derivation, all binarisations, initialisation tables for every `cabac_init_idc`. **Acc:** context-state trace matches a hand-derived reference for the first 200 macroblocks of each Main-profile conformance stream; every context index in Table 9-34 exercised | `vaco-codec-h264` | 15 §7.6 | 6.0 | T3-01c, D-03 | 4 |
| T3-01f | Macroblock layer: mb types and sub-mb types, partition geometry, neighbour availability derivation, skip and direct inference, MBAFF frame/field decision. **Acc:** MB-type and partition dump matches the reference for every stream in the base and Main corpora | `vaco-codec-h264` | 15 §7.6 | 5.0 | T3-01d, T3-01e | 4 |
| T3-01g | Intra prediction: 4×4, 8×8 and 16×16 luma modes, chroma modes, mode inference, constrained intra, I_PCM. **Acc:** all-intra conformance streams decode frame-exact; every prediction mode covered by a `vaco-checkasm` differential against D-09 | `vaco-codec-h264` | 15 §7.6 | 5.0 | T3-01f, D-09 | 4 |
| T3-01h | Inter prediction, derivation half: MV prediction and median/directional derivation, spatial and temporal direct modes, MV scaling, sub-mb partitioning. **Acc:** predicted-MV field matches a hand-derived reference over the B-slice conformance streams, including all direct-mode variants | `vaco-codec-h264` | 15 §7.6 | 4.0 | T3-01f, T3-01l | 4 |
| T3-01i | Motion compensation, sample half: six-tap qpel luma interpolation, bilinear chroma, weighted and implicit-weighted bi-prediction, over D-08 and PF-3.2's batched dispatch. **Acc:** every interpolation position bit-exact against the scalar reference across all tiers via `vaco-checkasm` | `vaco-codec-h264` | 15 §7.6 | 4.0 | T3-01f, D-08, PF-3.2 | 4 |
| T3-01j | Transform and dequantisation: 4×4 and 8×8 integer transforms, DC Hadamard for luma-16×16 and chroma, scaling-list application, lossless transform-bypass, qp derivation. **Acc:** bit-exact residual reconstruction against the specification's reference transform for every qp and scaling-list combination | `vaco-codec-h264` | 15 §7.6 | 4.0 | T3-01f, D-11 | 4 |
| T3-01k | Deblocking filter (§8.7): boundary-strength derivation, luma and chroma filtering, `disable_deblocking_filter_idc` modes, slice-boundary and MBAFF edge cases. **Acc:** deblocking output bit-exact against the spec reference over a synthetic edge-exhaustive corpus plus the conformance streams that stress BS derivation | `vaco-codec-h264` | 15 §7.6 | 4.0 | T3-01f, D-10 | 4 |
| T3-01l | Reference picture management and DPB: POC types 0/1/2, sliding-window and MMCO marking, `gaps_in_frame_num`, field/frame pairing, output-order bumping, `max_num_reorder_frames`. **Acc:** output picture order matches the reference exactly for every conformance stream, including the MMCO and long-term-reference cases | `vaco-codec-h264` | 15 §7.6 | 6.0 | T3-01c | 4 |
| T3-01m | Threading over F-03's banded picture model, High-profile extensions (4:2:2, 4:4:4, 8–14-bit, separate colour planes), and full-decoder integration. **Acc:** full-frame decode of the Main and High conformance sets is framemd5-identical to the reference, and byte-identical across all thread counts (plan 12 S10) | `vaco-codec-h264` | 15 §7.6 | 4.0 | T3-01g, T3-01h, T3-01i, T3-01j, T3-01k, T3-01l, F-03 | 4 |
| T3-01n | JVT conformance bring-up and triage; the D13 differential gate against the hardware decoders. **Acc:** the JVT conformance suite passes with every failure enumerated and owned; H-07's hw-vs-sw differential runs green | `vaco-codec-h264` | 15 §7.6 | 3.0 | T3-01m, X-05, QA-09 | 4 |
| **T3-02** *(group)* | HEVC decode, `patent-encumbered-hevc-decode`. **Never shipped** — **split into 14 children below; §11.** | `vaco-codec-hevc` | 15 §7.6 | **55.0** | D-03, D-08, D-09, D-10, D-18, F-03 | 4 |
| T3-02a | NAL layer and access-unit assembly: NAL-unit-type semantics, IRAP/RASL/RADL/CRA/BLA handling, temporal sub-layers, decoding-order and no-output rules, the crate's module map and interface freeze. **Acc:** AU boundaries and NAL classification match the reference for every stream in the JCT-VC base corpus; fuzz target on NAL scanning | `vaco-codec-hevc` | 15 §7.6 | 3.0 | D-18, F-02 | 4 |
| T3-02b | Parameter sets: VPS, SPS, PPS, scaling lists, PTL, VUI/HRD, short- and long-term reference picture set syntax. **Acc:** every parameter set in the corpus round-trips to the same field values P-01 reports; unsupported toolsets are rejected, not mis-decoded | `vaco-codec-hevc` | 15 §7.6 | 3.0 | T3-02a, P-01 | 4 |
| T3-02c | Slice-segment header: dependent slice segments, RPS derivation at slice level, tile and WPP entry-point offsets, slice-level tool flags. **Acc:** slice-header field dump matches the reference across the tiles, WPP and dependent-slice conformance streams | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02b | 4 |
| T3-02d | CABAC for HEVC on D-03's engine: context tables, initialisation for all init types, binarisations, WPP context save/restore, dependent-slice context inheritance. **Acc:** context-state trace matches a hand-derived reference for the first CTU row of each conformance stream, with WPP sync points verified explicitly | `vaco-codec-hevc` | 15 §7.6 | 7.0 | T3-02c, D-03 | 4 |
| T3-02e | CTU quadtree: CTB→CU→PU→TU partitioning, coding-tree traversal, z-scan and tile/slice scan order, mode info, part modes. **Acc:** partition-tree dump matches the reference for every conformance stream at every CTB size (16/32/64) | `vaco-codec-hevc` | 15 §7.6 | 5.0 | T3-02d | 4 |
| T3-02f | Intra prediction: 35 modes, angular prediction, reference-sample substitution, boundary smoothing, strong intra smoothing, constrained intra, intra 4:2:2 chroma derivation. **Acc:** all-intra conformance streams decode frame-exact; every mode exercised by a `vaco-checkasm` differential against D-09 | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02e, D-09 | 4 |
| T3-02g | Inter prediction, derivation half: merge and AMVP candidate lists, spatial and temporal MV candidates, MV scaling, combined bi-predictive and zero candidates, parallel-merge level. **Acc:** candidate lists match a hand-derived reference over the B-slice conformance streams at every `log2_parallel_merge_level` | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02e | 4 |
| T3-02h | Motion compensation, sample half: 8-tap luma and 4-tap chroma interpolation, weighted and bi-prediction, high-bit-depth intermediate precision, over D-08. **Acc:** every interpolation position bit-exact against the scalar reference across all tiers via `vaco-checkasm` | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02e, D-08 | 4 |
| T3-02i | Transform and dequantisation: DST-VII 4×4, DCT-II 4/8/16/32, scaling lists, transform-skip, RDPCM, cross-component prediction, transquant bypass. **Acc:** bit-exact residual reconstruction against the spec reference for every transform size, qp and scaling-list combination | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02e, SP-C10 | 4 |
| T3-02j | Deblocking filter: boundary-strength derivation on the 8×8 grid, luma/chroma filtering, slice and tile boundary controls. **Acc:** deblocking output bit-exact against the spec reference over a synthetic edge-exhaustive corpus plus the BS-stressing conformance streams | `vaco-codec-hevc` | 15 §7.6 | 3.0 | T3-02e, D-10 | 4 |
| T3-02k | SAO: band offset, edge offset (all four classes), SAO merge, per-component control, interaction with tile/slice boundaries. **Acc:** SAO output bit-exact against the spec reference over the SAO conformance streams; every offset class covered by a `vaco-checkasm` differential | `vaco-codec-hevc` | 15 §7.6 | 2.0 | T3-02e | 4 |
| T3-02l | DPB and RPS-driven reference management: POC derivation, RPS application, picture marking, output order, `no_output_of_prior_pics_flag`, CRA/BLA start-up behaviour. **Acc:** output picture order matches the reference exactly for every conformance stream, including random-access-start and broken-link cases | `vaco-codec-hevc` | 15 §7.6 | 4.0 | T3-02c | 4 |
| T3-02m | Tile, WPP and frame threading over F-03; range extensions (4:2:2, 4:4:4, 12-bit, extended precision); full-decoder integration. **Acc:** full-frame decode of the Main, Main10 and RExt conformance sets is framemd5-identical to the reference, and byte-identical across all thread counts | `vaco-codec-hevc` | 15 §7.6 | 5.0 | T3-02f, T3-02g, T3-02h, T3-02i, T3-02j, T3-02k, T3-02l, F-03 | 4 |
| T3-02n | JCT-VC conformance bring-up and triage; the D13 differential gate against the hardware decoders. **Acc:** the JCT-VC conformance suite passes with every failure enumerated and owned; H-07's hw-vs-sw differential runs green | `vaco-codec-hevc` | 15 §7.6 | 3.0 | T3-02m, X-05, QA-09 | 4 |
| **T3-03** *(group)* | AAC-LC/HE/HEv2 decode, `patent-encumbered-aac-decode`. **Never shipped.** AAC *remuxing* stays in the default build (D9) and is already delivered by P-03 — **split into 6 children below; §11.** | `vaco-codec-aac` | 15 §7.6 | **30.0** | SP-C6, D-06, D-01 | 4 |
| T3-03a | Configuration layer: `AudioSpecificConfig`, program config element, channel configuration, ADTS/LATM handover from P-03, object-type gating. **Acc:** every configuration in the ISO 14496-26 vector set maps to the same channel layout and sample rate the reference reports | `vaco-codec-aac` | 15 §7.6 | 3.0 | P-03, F-02 | 4 |
| T3-03b | AAC-LC core syntax: window sequences and shapes, scalefactor bands, section data, scalefactor decode, spectral Huffman decode, pulse data. **Acc:** decoded spectral-coefficient arrays are bit-exact against hand-checked vectors for every window sequence and codebook | `vaco-codec-aac` | 15 §7.6 | 7.0 | T3-03a, D-01 | 4 |
| T3-03c | Reconstruction: inverse quantisation, TNS, joint stereo (M/S and intensity), LTP/main prediction, IMDCT and windowing/overlap-add over SP-C6. **Acc:** the LC conformance vectors decode within the specification's RMS tolerance and bit-exactly where the fixed-point contract applies | `vaco-codec-aac` | 15 §7.6 | 7.0 | T3-03b, SP-C6, D-06 | 4 |
| T3-03d | SBR: analysis and synthesis QMF banks, envelope and noise-floor decode, HF generation and patching, envelope adjustment. **Acc:** the HE-AAC conformance vectors decode within tolerance; the QMF banks are covered by a `vaco-checkasm` differential | `vaco-codec-aac` | 15 §7.6 | 7.0 | T3-03a, SP-C6 | 4 |
| T3-03e | Parametric Stereo (HE-AACv2): decorrelation, stereo reconstruction, IID/ICC/IPD/OPD parameter decode. **Acc:** the HE-AACv2 conformance vectors decode within tolerance | `vaco-codec-aac` | 15 §7.6 | 4.0 | T3-03d | 4 |
| T3-03f | Conformance, multichannel and error-resilience posture: ISO 14496-26 vector matrix, downmix behaviour, unsupported-toolset rejection. **Acc:** the purchased conformance vector set passes with every divergence enumerated and owned | `vaco-codec-aac` | 15 §7.6 | 2.0 | T3-03c, T3-03e, QA-09 | 4 |
| T3-04 | AAC-LC encode | `vaco-codec-aac` | 15 §7.6 | 25.0 | T3-03, X-04 | post-1.0 |
| T3-05 | AC-3 / E-AC-3 encode | `vaco-codec-ac3` | 15 §7.6 | 12.0 | T2-04, X-04 | post-1.0 |
| T3-06 | DTS core decode | `vaco-codec-dts` | 15 §7.6 | 40.0 | SP-C6 | post-1.0 |
| T3-07 | VVC decode. **Plan 15 §5.3 recommendation 4: do not build it.** 110 pw for a RED codec with negligible deployment. Listed so its absence surprises nobody. | `vaco-codec-vvc` | 15 §7.6 | 110.0 | T3-02 | post-1.0 |
| T4-* | The documented long tail, grouped into ~10 crates | `vaco-codec-legacy-*` | 15 §7.6 | ~250.0 | varies | post-1.0 |
| T5-01 | The two-team clean-room programme for the ~15 high-value spec-less formats (TrueHD/MLP, WavPack, Monkey's Audio, TTA, DTS core, ProRes, Bink, Smacker, RealVideo 3/4, ATRAC3/3+, WMA v1/v2, QuickTime RLE/Animation, MS Video 1, Cinepak, HuffYUV/FFVHuff, UtVideo). ~120 pw at a 2.5× clean-room multiplier. | `vaco-codec-legacy-*` | 15 §3.5 | ~300.0 | — | post-1.0 |
| | **Subtotal (T3-01..03, in v1.0)** | | | **145.0** | | |
| | **Subtotal (post-1.0)** | | | **~737.0** | | |

### 1.12 Filters (`FT-`, plan 16 §8)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| FT-1.1 | Pad/link model, `FrameQueue`, `AudioFifo`, `QueueBudget`, link stats | `vaco-filter-core` | 16 §8.1 | 2.0 | FD-11, FD-02 | 2 |
| FT-1.2 | `Filter` trait, `FilterCtx`, node/link arenas, the split-borrow driver | `vaco-filter-core` | 16 §8.1 | 1.5 | FT-1.1 | 2 |
| FT-1.3 | Readiness scheduler, `run_once`/`run`, quiescence classifier | `vaco-filter-core` | 16 §8.1 | 2.0 | FT-1.2 | 2 |
| FT-1.4 | Status/EOF propagation, timestamp rules, generic forwarding helpers | `vaco-filter-core` | 16 §8.1 | 1.5 | FT-1.3 | 2 |
| FT-1.5 | Format negotiation: `Constraint`, union-find equality, intersect, repair, loss function, PICK, configure | `vaco-filter-core` | 16 §8.1 | 3.5 | FT-1.2, FD-07, FD-10 | 2 |
| FT-1.6 | Negotiation diagnostics + provenance + snapshot tests | `vaco-filter-core` | 16 §8.1 | 1.0 | FT-1.5 | 2 |
| FT-1.7 | Adapters: `Simple`, `SourceFilter`, `AudioFilter`. **Every one of ~560 filters is written against these — an awkward API here is paid for 560 times.** | `vaco-filter-core` | 16 §8.1 | 2.0 | FT-1.4 | 2 |
| FT-1.8 | `SliceFilter` adapter, band splitting, rayon pool, deterministic reductions | `vaco-filter-core` | 16 §8.1 | 2.0 | FT-1.7 | 2 |
| FT-1.9 | Timeline `enable=`, `vaco-expr` integration | `vaco-filter-core` | 16 §8.1 | 1.0 | FT-1.7, FD-06 | 2 |
| FT-1.10 | Command dispatch, option-schema default impl, queued commands | `vaco-filter-core` | 16 §8.1 | 1.0 | FT-1.7 | 2 |
| FT-1.11 | Buffer sources/sinks, `SourceHandle`/`SinkHandle`, sink constraints | `vaco-filter-core` | 16 §8.1 | 1.5 | FT-1.4 | 2 |
| FT-1.12 | Multi-input timestamp alignment + adapter + the option truth table (used by ~68 filters) | `vaco-filter-framesync` | 16 §8.1 | 2.5 | FT-1.7 | 2 |
| FT-2.1 | `next_token` escaping scanner + the three-level test-vector corpus | `vaco-filter-graph` | 16 §8.2 | 1.5 | — | 2 |
| FT-2.2 | AST, chain/filter/label parsing, spans, caret diagnostics | `vaco-filter-graph` | 16 §8.2 | 1.5 | FT-2.1 | 2 |
| FT-2.3 | Instantiation, option binding (positional + `key=value`), dynamic pads | `vaco-filter-graph` | 16 §8.2 | 1.5 | FT-2.2, FD-05 | 2 |
| FT-2.4 | Link resolution (labels + unlabeled auto-connect), open-pad export | `vaco-filter-graph` | 16 §8.2 | 1.5 | FT-2.3 | 2 |
| FT-2.5 | Validation checks + messages, Kahn sort, cycle detection | `vaco-filter-graph` | 16 §8.2 | 1.0 | FT-2.4 | 2 |
| FT-2.6 | `ConverterFactory` policy, coalescing, `sws_flags=` plumbing, `auto_*` naming | `vaco-filter-graph` | 16 §8.2 | 1.5 | FT-2.5, FT-1.5 | 2 |
| FT-2.7 | `to_dot`, `dump`, round-trip property tests | `vaco-filter-graph` | 16 §8.2 | 1.0 | FT-2.2 | 2 |
| FT-3.1 | Colour parsing, format-aware fill/blend/box, subsampled + high-bit-depth paths | `vaco-filter-draw` | 16 §8.3 | 3.0 | FT-1.7, FD-07 | 4 |
| FT-3.2 | `scene_sad`, `edge_common`, box-blur core, SAD/hadamard, integral images | `vaco-filter-vdsp` | 16 §8.3 | 3.0 | FT-1.7, PF-0.1 | 4 |
| FT-3.3 | Motion estimation, affine transform, LUT sampling, morphology core | `vaco-filter-vdsp` | 16 §8.3 | 3.0 | FT-3.2 | 4 |
| FT-3.4 | Biquad design, wave tables, windows, EBU R128 core, partitioned FIR | `vaco-filter-adsp` | 16 §8.3 | 3.5 | FT-1.7, SP-C6 | 4 |
| FT-3.5 | `TextRenderer`, fontdb + alias table, shaping/glyph caches, mask rasterisation. **Blocked on the `rustybuzz` provenance question** (§8 item 6). | `vaco-filter-text` | 16 §8.3 | 4.0 | FT-3.1 | 4 |
| **FT-4.1** *(group)* | **T1 video (13)** — **three crates, split for single-writer ownership** — **split into 3 children below; §11.** | `vaco-filter-scale`, `-crop`, `-overlay` | 16 §8.4 | **5.0** | FT-1.7, SP-A11 | 4 |
| FT-4.1a | `scale`, `format`, `noformat`, `setsar`, `setdar`, `setparams`. **Acc:** each filter's argument-vector corpus is framecrc-identical to the reference through FT-6.1 | `vaco-filter-scale` | 16 §8.4 | 2.0 | FT-1.7, SP-A11 | 4 |
| FT-4.1b | `crop`, `pad`, `transpose`, `hflip`, `vflip`. **Acc:** each filter's argument-vector corpus is framecrc-identical to the reference | `vaco-filter-crop` | 16 §8.4 | 1.5 | FT-1.7 | 4 |
| FT-4.1c | `overlay`, `fps`. **Acc:** both filters' argument-vector corpora are framecrc-identical to the reference, including the framesync option truth table | `vaco-filter-overlay`, `vaco-filter-fps` | 16 §8.4 | 1.5 | FT-1.7, FT-1.12 | 4 |
| FT-4.2 | **T1 audio (11)**: `aresample`, `aformat`, `volume`, `amix`, `amerge`, `channelmap`, `channelsplit`, `join`, `pan`, `asetnsamples`, `asetrate` | `vaco-filter-audio-*` | 16 §8.4 | 4.0 | FT-1.7, SP-B8 | 4 |
| FT-4.3 | **T1 plumbing + sources/sinks + trim/concat (24)** | `vaco-filter-plumbing`, `-source` | 16 §8.4 | 4.0 | FT-1.11 | 4 |
| FT-4.4 | **T2 colour + LUT (~34)** | `vaco-filter-color` | 16 §8.4 | 8.0 | FT-1.7, SP-A5 | 4 |
| FT-4.5 | **T2 deinterlace** (yadif, bwdif, w3fdif, estdif, fieldmatch, decimate, telecine family). **Blocked on a spec-writer producing `planning/spec/deinterlace.md`** (plan 16 §9.5). | `vaco-filter-deinterlace` | 16 §8.4 | 6.0 | FT-1.8 | 4 |
| **FT-4.6** *(group)* | **T2 blur/sharpen/convolve (~28)** — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-filter-blur`, `-denoise` | 16 §8.4 | **6.0** | FT-3.2 | 4 |
| FT-4.6a | Blur, sharpen and convolution filters (`gblur`, `boxblur`, `unsharp`, `smartblur`, `convolution`, `sobel`, …). **Acc:** every filter in the group is framecrc-identical to the reference over its argument-vector corpus | `vaco-filter-blur` | 16 §8.4 | 3.0 | FT-3.2 | 4 |
| FT-4.6b | Denoise filters (`hqdn3d`, `atadenoise`, `removegrain`, `nlmeans`, `owdenoise`, …). **Acc:** every filter in the group is framecrc-identical to the reference over its argument-vector corpus | `vaco-filter-denoise` | 16 §8.4 | 3.0 | FT-3.2 | 4 |
| FT-4.7 | **T2 geometry (~28)** | `vaco-filter-geometry` | 16 §8.4 | 6.0 | FT-3.3 | 4 |
| **FT-4.8** *(group)* | **T2 audio EQ + dynamics (~40)** — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-filter-audio-eq`, `-dynamics` | 16 §8.4 | **9.0** | FT-3.4 | 4 |
| FT-4.8a | The biquad family (12 filters from one file) plus `anequalizer`, `firequalizer`, `superequalizer`. **Acc:** every filter is sample-exact against the reference over its argument-vector corpus, or inside the published tolerance with the divergence recorded | `vaco-filter-audio-eq` | 16 §8.4 | 5.0 | FT-3.4 | 4 |
| FT-4.8b | Dynamics: compressor/limiter/gate/expander/sidechain family plus `loudnorm` and `dynaudnorm`. **Acc:** every filter is inside the published tolerance against the reference, and `loudnorm` reproduces the reference's EBU R128 measurements | `vaco-filter-audio-dynamics` | 16 §8.4 | 4.0 | FT-3.4 | 4 |
| FT-4.9 | **T2 analysis/metrics** (psnr, ssim, vif, xpsnr, signalstats, …) | `vaco-filter-analysis` | 16 §8.4 | 5.0 | FT-1.12 | 4 |
| FT-4.10 | **T2 text/drawing** (drawtext, drawbox, drawgrid, drawgraph) | `vaco-filter-text` | 16 §8.4 | 4.0 | FT-3.5 | 4 |
| FT-4.11 | **T2 palette/GIF, stack, overlay family, temporal** | `vaco-filter-palette`, `-stack` | 16 §8.4 | 6.0 | FT-1.12 | 4 |
| **FT-4.12** *(group)* | **T3 video long tail (~150)** — the most parallelisable work in the project; **split into seven dispatchable groups** — **split into 7 children below; §11.** | `vaco-filter-*` | 16 §8.4 | **34.0** | FT-1.7 | 4 |
| FT-4.12a | T3 video generators and test sources (~20). **Acc:** every filter in the group is framecrc-identical to the reference over its argument-vector corpus | `vaco-filter-source-*` | 16 §8.4 | 5.0 | FT-1.11 | 4 |
| FT-4.12b | T3 frame-rate, temporal and interleave filters (~20). **Acc:** every filter in the group is framecrc-identical to the reference, with timestamp behaviour verified against the reference | `vaco-filter-temporal-*` | 16 §8.4 | 5.0 | FT-1.7 | 4 |
| FT-4.12c | T3 pixel-format, bit-depth and component filters (~20). **Acc:** every filter in the group is framecrc-identical to the reference at every supported pixel format | `vaco-filter-component-*` | 16 §8.4 | 5.0 | FT-1.7 | 4 |
| FT-4.12d | T3 video analysis and detection filters (~20). **Acc:** every filter in the group emits metadata identical to the reference over its argument-vector corpus | `vaco-filter-detect-*` | 16 §8.4 | 5.0 | FT-1.7 | 4 |
| FT-4.12e | T3 effects and stylisation filters (~25). **Acc:** every filter in the group is framecrc-identical to the reference over its argument-vector corpus | `vaco-filter-effect-*` | 16 §8.4 | 5.0 | FT-1.7 | 4 |
| FT-4.12f | T3 metadata, side-data and timeline filters (~20). **Acc:** every filter in the group produces metadata and timeline behaviour identical to the reference | `vaco-filter-meta-*` | 16 §8.4 | 4.0 | FT-1.9 | 4 |
| FT-4.12g | T3 remaining miscellaneous video filters (~25). **Acc:** every filter in the group is framecrc-identical to the reference over its argument-vector corpus | `vaco-filter-*` | 16 §8.4 | 5.0 | FT-1.7 | 4 |
| **FT-4.13** *(group)* | **T3 audio long tail (~64)** — **split into five dispatchable groups** — **split into 5 children below; §11.** | `vaco-filter-*` | 16 §8.4 | **14.0** | FT-1.7 | 4 |
| FT-4.13a | T3 audio generators and sources (~12). **Acc:** every filter in the group is sample-exact against the reference over its argument-vector corpus | `vaco-filter-asource-*` | 16 §8.4 | 3.0 | FT-1.11 | 4 |
| FT-4.13b | T3 channel, layout and mixing filters (~14). **Acc:** every filter in the group is sample-exact against the reference over its argument-vector corpus | `vaco-filter-achannel-*` | 16 §8.4 | 3.0 | FT-1.7 | 4 |
| FT-4.13c | T3 audio analysis and measurement filters (~14). **Acc:** every filter in the group emits metadata identical to the reference over its argument-vector corpus | `vaco-filter-ameasure-*` | 16 §8.4 | 3.0 | FT-1.7 | 4 |
| FT-4.13d | T3 audio effects and modulation filters (~14). **Acc:** every filter in the group is inside the published tolerance against the reference | `vaco-filter-aeffect-*` | 16 §8.4 | 3.0 | FT-1.7 | 4 |
| FT-4.13e | T3 remaining miscellaneous audio filters (~10). **Acc:** every filter in the group is inside the published tolerance against the reference | `vaco-filter-*` | 16 §8.4 | 2.0 | FT-1.7 | 4 |
| FT-5.1 | Bitmap + simple-text subtitle rendering | `vaco-filter-subtitle` | 16 §8.5 | 1.5 | FT-3.1, T2-13 | 4 |
| FT-5.2 | ASS stage (a): parsing, styles, static tags | `vaco-ass` | 16 §8.5 | 4.0 | FT-3.5 | 4 |
| FT-5.3 | ASS stage (b): animation, karaoke, `\p` drawing, 3-D rotation | `vaco-ass` | 16 §8.5 | 7.0 | FT-5.2 | 4 |
| FT-5.4 | ASS visual differential harness (per-frame SSIM gate) + divergence list | `vaco-ass` tests | 16 §8.5 | 2.0 | FT-5.3, X-04 | 4 |
| FT-5.5 | GPU device/frame model, negotiation integration, hwupload/hwdownload, encoder batching | `vaco-filter-gpu` | 16 §8.5 | 4.0 | FT-1.5, H-01 | 4 |
| **FT-5.6** *(group)* | **16 WGSL kernels** + CPU-counterpart differential gates — replaces ~87 upstream per-vendor filter variants — **split into 3 children below; §11.** | `vaco-filter-gpu` | 16 §8.5 | **10.0** | FT-5.5 | 4 |
| FT-5.6a | Kernel-authoring harness: WGSL module layout, bind-group conventions, the CPU-counterpart differential gate, and one reference kernel end-to-end. **Acc:** the reference kernel passes the CPU differential at the published tolerance and the harness is documented for the remaining fifteen | `vaco-filter-gpu` | 16 §8.5 | 3.0 | FT-5.5 | 4 |
| FT-5.6b | Colour and format kernels: scale, format conversion, colour matrix, tonemap. **Acc:** each kernel passes the CPU-counterpart differential at the published tolerance | `vaco-filter-gpu` | 16 §8.5 | 4.0 | FT-5.6a | 4 |
| FT-5.6c | Spatial and temporal kernels: blur, sharpen, overlay/blend, deinterlace, transpose/pad/crop. **Acc:** each kernel passes the CPU-counterpart differential at the published tolerance | `vaco-filter-gpu` | 16 §8.5 | 3.0 | FT-5.6a | 4 |
| **FT-5.7** *(group)* | VMAF implementation + validation against published scores. **Blocked on §8 item 7.** Schedule late — **split into 3 children below; §11.** | `vaco-filter-analysis` | 16 §8.5 | **10.0** | FT-4.9 | 4 |
| FT-5.7a | Elementary features: VIF at four scales, ADM/DLM, motion. **Acc:** each feature reproduces the published per-frame feature values on the reference clips within the stated tolerance | `vaco-filter-analysis` | 16 §8.5 | 5.0 | FT-4.9 | 4 |
| FT-5.7b | SVM model loading, score fusion, temporal pooling. **Acc:** the default model reproduces published VMAF scores on the reference clips within the stated tolerance | `vaco-filter-analysis` | 16 §8.5 | 3.0 | FT-5.7a | 4 |
| FT-5.7c | Model variants (4K, phone), confidence interval, filter option surface. **Acc:** each variant reproduces its published scores and the option surface matches the reference's | `vaco-filter-analysis` | 16 §8.5 | 2.0 | FT-5.7b | 4 |
| FT-5.8 | `v360` | `vaco-filter-geometry` | 16 §8.5 | 4.0 | FT-3.3 | 4 |
| FT-5.9 | Stabilisation (`stabdetect`/`stabtransform`, `deshake`) | `vaco-filter-stabilise` | 16 §8.5 | 5.0 | FT-3.3 | 4 |
| FT-5.10 | DNN filters over `tract`, behind a non-default feature. **Gate 3 assessment of `tract` required first.** | `vaco-filter-dnn` | 16 §8.5 | 4.0 | FT-1.7 | 4 |
| FT-6.1 | Differential harness integration: per-filter argument-vector corpus, framecrc comparison, allowlist management | `vaco-conformance` | 16 §8.6 | 4.0 | QA-02 | 4 |
| FT-6.2 | Fuzz targets: the graph-string parser (**highest value — it is the only attacker-reachable text parser in the subsystem**), option parsing, per-filter frame fuzzing | fuzz | 16 §8.6 | 3.0 | FT-2.5, QA-05 | 4 |
| FT-6.3 | Per-filter criterion suites with CI regression tracking | benches | 16 §8.6 | 3.0 | PF-0.6 | 4 |
| FT-6.4 | `docs/filter/*.md` per crate | docs | 16 §8.6 | 4.0 | all FT | 4 |
| | **Subtotal** | | | **224.0** *(plan states 228.0 — see §2.3)* | | |

### 1.13 Signal processing (`SP-`, plan 17)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| SP-A1 | Op vocabulary, `Graph`, `Block`, scalar per-op kernels | `vaco-scale` | 17 A.18 | 4.0 | FD-07, FD-10 | 2 |
| SP-A2 | Graph construction: format decomposition, canonical sequence | `vaco-scale` | 17 A.18 | 3.0 | SP-A1 | 2 |
| SP-A3 | Optimiser passes (§A.5.1–A.5.9) | `vaco-scale` | 17 A.18 | 4.0 | SP-A1, SP-A2 | 2 |
| SP-A4 | Filter coefficient generators (all ten kernels + banks) | `vaco-scale` | 17 A.18 | 3.0 | SP-A1 | 2 |
| SP-A5 | Colour: matrices, transfer functions, primaries, range | `vaco-scale` | 17 A.18 | 4.0 | FD-10, SP-A1 | 2 |
| SP-A6 | Tone mapping, intents, 3D LUT, tetrahedral interpolation | `vaco-scale` | 17 A.18 | 3.0 | SP-A5 | 2 |
| SP-A7 | Dither + alpha | `vaco-scale` | 17 A.18 | 2.0 | SP-A1 | 2 |
| SP-A8 | Chain compiler + dispatch + fused-kernel patterns | `vaco-scale` | 17 A.18 | 3.0 | SP-A1, SP-A3 | 2 |
| SP-A9 | SIMD kernels, per-op, all tiers *(absorbs PF-1.1/1.2/1.3)* | `vaco-scale` | 17 A.18 | 6.0 | SP-A1, SP-A8, PF-0.1 | 2 |
| SP-A10 | SIMD kernels, fused patterns, profile-driven | `vaco-scale` | 17 A.18 | 3.0 | SP-A9 | 2 |
| SP-A11 | Frame + slice APIs, slice threading, buffer pooling | `vaco-scale` | 17 A.18 | 2.0 | SP-A2, SP-A8 | 2 |
| SP-A12 | Option surface, parsing, legacy-flag normalisation | `vaco-scale` | 17 A.18 | 1.5 | FD-05 | 2 |
| SP-A13 | Property tests, differential harness integration, fuzz targets. **Must start with A2, not after A10.** The optimiser-soundness property test is the highest-value test in the crate. | `vaco-scale` tests | 17 A.18 | 4.0 | SP-A2, X-01 | 2 |
| SP-A14 | **Fidelity probes**: determine and pin each free parameter in §A.15.1 classes 2–8, with recorded provenance. ~20 parameters, easy to under-budget. | `vaco-scale` probes | 17 A.18 | 3.0 | SP-A13 | 2 |
| SP-A15 | Benchmarks + CI regression tracking | benches | 17 A.18 | 1.5 | SP-A11, PF-0.6 | 2 |
| SP-A16 | `docs/scale/*.md` | docs | 17 A.18 | 1.5 | all SP-A | 2 |
| SP-B1 | Buffer model, `Stage` trait, `Resampler` composition, builder | `vaco-resample` | 17 B.16 | 1.5 | FD-08 | 2 |
| SP-B2 | Sample-format conversion: 30 element converters, interleave/deinterleave, scalar reference | `vaco-resample` | 17 B.16 | 2.0 | SP-B1 | 2 |
| SP-B3 | Format-conversion SIMD, all tiers *(absorbs PF-1.4)* | `vaco-resample` | 17 B.16 | 2.0 | SP-B2, PF-0.1 | 2 |
| SP-B4 | Rematrix: matrix construction, layout mapping rules, normalisation | `vaco-resample` | 17 B.16 | 2.5 | SP-B1 | 2 |
| SP-B5 | Rematrix: matrix encodings (Dolby, DPLII/x/z, Ex) | `vaco-resample` | 17 B.16 | 1.0 | SP-B4 | 2 |
| SP-B6 | Rematrix kernels + SIMD, all shapes | `vaco-resample` | 17 B.16 | 1.5 | SP-B4, SP-B3 | 2 |
| SP-B7 | Filter design: windows, cutoff derivation, bank generation, normalisation | `vaco-resample` | 17 B.16 | 2.5 | SP-B1 | 2 |
| SP-B8 | Polyphase engine: phase advance, exact-rational, linear interp, delay/priming/drain | `vaco-resample` | 17 B.16 | 3.0 | SP-B7 | 2 |
| SP-B9 | Convolution kernels + SIMD, integer and float paths *(absorbs PF-1.5)* | `vaco-resample` | 17 B.16 | 2.5 | SP-B8, PF-0.1 | 2 |
| SP-B10 | Dither: all methods, PRNG, noise-shaping curve generation and validation *(absorbs PF-1.6)* | `vaco-resample` | 17 B.16 | 2.5 | SP-B1 | 2 |
| SP-B11 | Timestamp compensation: soft, hard, async, `first_pts`, the manual API | `vaco-resample` | 17 B.16 | 2.0 | SP-B8 | 2 |
| SP-B12 | Option surface, parsing, `soxr` compatibility handling | `vaco-resample` | 17 B.16 | 1.0 | FD-05 | 2 |
| SP-B13 | Property tests, signal-quality suite, differential harness, fuzz | `vaco-resample` tests | 17 B.16 | 3.5 | SP-B1, X-01 | 2 |
| SP-B14 | **Fidelity probes**: pin classes 1–7 and 11; investigate class 8. **The item most likely to overrun** — scheduled with slack, defined fallback. | `vaco-resample` probes | 17 B.16 | 3.0 | SP-B13, SP-B9 | 2 |
| SP-B15 | Benchmarks + CI regression tracking | benches | 17 B.16 | 1.0 | SP-B9, PF-0.6 | 2 |
| SP-B16 | `docs/resample/*.md` | docs | 17 B.16 | 1.0 | all SP-B | 2 |
| SP-C1 | API, `Plan`/`Tx` split, decomposition types, error taxonomy | `vaco-tx` | 17 C.12 | 1.0 | FD-01 | 2 |
| SP-C2 | Small-radix kernels (2,3,4,5,7,8), scalar, all precisions | `vaco-tx` | 17 C.12 | 1.5 | SP-C1 | 2 |
| SP-C3 | Split-radix power-of-two FFT, scalar reference | `vaco-tx` | 17 C.12 | 1.5 | SP-C2 | 2 |
| SP-C4 | Mixed-radix + prime-factor composition and the decomposition selector | `vaco-tx` | 17 C.12 | 2.0 | SP-C2, SP-C3 | 2 |
| SP-C5 | Rader + Bluestein fallbacks | `vaco-tx` | 17 C.12 | 1.5 | SP-C4 | 2 |
| SP-C6 | Derived transforms: MDCT/IMDCT (+`FULL_IMDCT`), RDFT (+R2R/R2I), DCT-II/III, DCT-I, DST-I. **Blocks every transform-coded audio codec.** | `vaco-tx` | 17 C.12 | 3.0 | SP-C3 | 2 |
| SP-C7 | Split-complex layout, twiddle generation and ordering, boundary conversion | `vaco-tx` | 17 C.12 | 1.5 | SP-C3 | 2 |
| SP-C8 | SIMD kernels: butterflies, cmul, all tiers, all widths *(absorbs PF-3.4)* | `vaco-tx` | 17 C.12 | 4.0 | SP-C7, PF-0.1 | 2 |
| SP-C9 | SIMD last-stage strategy (§C.6.3): measure, then implement the winner | `vaco-tx` | 17 C.12 | 2.0 | SP-C8 | 2 |
| SP-C10 | **`i32` fixed-point paths**: the arithmetic contract, all kinds, golden vectors, conformance validation. `docs/tx/fixed-point.md` is **normative for codec conformance**, not documentation. | `vaco-tx` | 17 C.12 | 4.0 | SP-C3, SP-C6 | 2 |
| SP-C11 | Property tests, direct-DFT oracle, `rustfft` oracle, fuzz targets | `vaco-tx` tests | 17 C.12 | 2.5 | SP-C1 | 2 |
| SP-C12 | Benchmarks + CI regression tracking, incl. the §C.6.3 measurement | benches | 17 C.12 | 1.5 | SP-C8, PF-0.6 | 2 |
| SP-C13 | Cache-blocked large transforms — **explicitly deferrable.** Vorbis at N=8192 is the only realistic trigger. | `vaco-tx` | 17 C.12 | 2.0 *(deferred)* | SP-C8 | post-1.0 |
| SP-C14 | `docs/tx/*.md` incl. the arithmetic contract | docs | 17 C.12 | 1.0 | all SP-C | 2 |
| | **Subtotal (excl. deferred SP-C13)** | | | **108.0** *(plan states 113.0)* | | |

### 1.14 I/O, protocols, format framework (`IO-`, `FW-`, `SH-`, `PR-`, plan 18 §8)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| IO-01 | `IoContext` read + write, buffering, short seek, sticky state, checksums. **Blocks every format. Freeze the surface after review.** | `vaco-io` | 18 §8.1 | 4.0 | FD-01, QA-01 | 2 |
| IO-02 | `DynBuf`, `DataMarker` typed writes, cancellation. **`DynBuf` blocks every muxer.** | `vaco-io` | 18 §8.1 | 2.0 | IO-01 | 2 |
| IO-03 | URL grammar, dispatch, whitelist/blacklist, nested-open depth. **W1–W4 are security properties; review hard.** | `vaco-protocol-core` | 18 §8.1 | 3.0 | IO-01 | 2 |
| FW-01 | Object model §1.1: `Stream`, `Program`, `Chapter`, `StreamGroup`, `Metadata`, dispositions, side data. **Blocks everything. Do first, review hard, then freeze.** Depends on F-01 per D14.1. | `vaco-format-core` | 18 §8.1 | 3.0 | F-01 | 2 |
| FW-02 | `Demuxer`/`Muxer` traits, `DemuxCtx`/`MuxCtx`, descriptors, `ParserProvider`/`BsfProvider` seams, registry codegen. **The D14.1 layering amendment lands here.** | `vaco-format-core` | 18 §8.1 | 2.0 | FW-01 | 2 |
| FW-03 | Probing §1.5: padded `ProbeData`, scoring, retry, forced format, whitelist, tie-break table + `just calibrate-probe` | `vaco-format-core` | 18 §8.1 | 3.0 | FW-02 | 2 |
| FW-04 | Stream discovery §1.6: the loop, limits, DD1–DD4, per-format analyse-duration defaults | `vaco-format-core` | 18 §8.2 | 5.0 | FW-03 | 2 |
| FW-05 | Timestamp model R1–R13: rescaling, NOPTS, wrap state, `start_time` | `vaco-format-core` | 18 §8.2 | 4.0 | FW-01 | 2 |
| FW-06 | Timestamp model R14–R24: duration estimation, generation, monotonic repair, fill-in | `vaco-format-core` | 18 §8.2 | 4.0 | FW-05, FW-04 | 2 |
| FW-07 | Seek §1.8: index, generic seek, binary search, byte seek, flags, the `-ss` contract | `vaco-format-core` | 18 §8.2 | 6.0 | FW-04, FW-05 | 2 |
| FW-08 | Muxer core: init/header/packet/trailer state machine, the §1.7 mux chain M1–M7, `avoid_negative_ts`, monotonicity | `vaco-format-core` | 18 §8.2 | 4.0 | FW-02, IO-02 | 4 |
| FW-09 | Interleaving §1.9: per-DTS, chunked, sparse escape, custom policies | `vaco-format-core` | 18 §8.2 | 4.0 | FW-08 | 4 |
| FW-10 | BSF-in-muxer §1.10 | `vaco-format-core` | 18 §8.2 | 2.0 | FW-08, B-01 | 4 |
| FW-11 | The 39 generic options + `vaco-opts` wiring + `-h demuxer=`/`-h muxer=` introspection | `vaco-format-core` | 18 §8.1 | 2.0 | FW-02, FD-05 | 2 |
| FW-12 | Metadata/chapter/program/stream-group model + `MetadataConv` driver | `vaco-format-metadata` | 18 §8.2 | 3.0 | FW-01 | 2 |
| SH-01 | ISO base media file format shared tables and helpers | `vaco-format-isom` | 18 §8.3 | 4.0 | FW-02 | 2 |
| SH-02 | RIFF shared tables and helpers | `vaco-format-riff` | 18 §8.3 | 3.0 | FW-02 | 2 |
| SH-03 | MPEG-TS PSI tables, descriptors, CRC | `vaco-format-mpegts-tables` | 18 §8.3 | 3.0 | FW-02 | 2 |
| SH-04 | Start codes, PES, 33-bit timestamps, SCR/PCR | `vaco-format-mpeg-common` | 18 §8.3 | 2.0 | FW-02, FD-03 | 2 |
| SH-05 | NALU config records, Annex-B ↔ length-prefixed | `vaco-format-nalu` | 18 §8.3 | 1.5 *[−1.5 → FD-03/P-01]* | FW-02, FD-03 | 2 |
| SH-06 | ID3 (wraps `id3`, D11 boundary, our conversion table) | `vaco-format-id3` | 18 §8.3 | 3.0 | FW-12 | 4 |
| SH-07 | Vorbis comment + FLAC picture | `vaco-format-vorbiscomment` | 18 §8.3 | 1.5 | FW-12 | 4 |
| SH-08 | APE tag **+ ReplayGain** — **SH-10 merged in, §11.5** | `vaco-format-apetag`, `vaco-format-replaygain` | 18 §8.3 | 1.5 | FW-12 | 4 |
| SH-09 | Language code tables | `vaco-format-avlanguage` | 18 §8.3 | 1.0 | FW-01 | 2 |
| SH-10 | ~~ReplayGain~~ **[merged → SH-08, §11.5]** | `vaco-format-replaygain` | 18 §8.3 | 0.0 | — | 4 |
| PR-01 | `file`, `pipe`, `fd`, `data`, `md5` | `vaco-protocol-*` | 18 §8.4 | 1.5 | IO-03 | 2 |
| PR-02 | `cache`, `subfile`, `concat`, `concatf`, `tee`, `async` | `vaco-protocol-*` | 18 §8.6 | 5.0 | IO-03 | 4 |
| PR-03 | `crypto` (AES-CTR over a nested URL) | `vaco-protocol-crypto` | 18 §8.6 | 1.5 | IO-03 | 4 |
| PR-04 | `tcp`, `udp`, `udplite`, `unix` (+ `socket2`, permitted by D14.3) | `vaco-protocol-*` | 18 §8.6 | 4.5 | IO-03 | 4 |
| PR-05 | `tls` via `rustls` + **the D14.2 provider decision** + root store. **Benchmark `rustls-rustcrypto` in wave 2, not wave 4** — it gates 53.5 pw downstream. | `vaco-protocol-tls` | 18 §8.6 | 3.0 | PR-04 | 4 |
| PR-06 | `http`/`https` (wrapping `ureq`) + range/seek/reconnect/ICY/persistent/chunked-POST | `vaco-protocol-http` | 18 §8.6 | 6.0 | PR-05 | 4 |
| PR-07 | `httpproxy`, `ftp`, `gopher`, `gophers`, `icecast`, `ipfs_gateway`, `ipns_gateway` | `vaco-protocol-*` | 18 §8.6 | 4.5 | PR-06 | 4 |
| PR-08 | `rtp`, `srtp`, `prompeg` | `vaco-protocol-rtp` | 18 §8.6 | 5.0 | PR-04 | 4 |
| **PR-09** *(group)* | **`rtmp`/`rtmps`/`rtmpt`/`ffrtmphttp` native, from the Adobe specification** — **split into 3 children below; §11.** | `vaco-protocol-rtmp` | 18 §8.6 | **10.0** | PR-05 | 4 |
| PR-09a | Chunk stream layer: handshake (simple and complex), chunking/dechunking, control messages, window/bandwidth. **Acc:** a live handshake and chunk exchange against a reference server completes and the fuzz target is green for 24 h | `vaco-protocol-rtmp` | 18 §8.6 | 4.0 | PR-05 | 4 |
| PR-09b | AMF0/AMF3 encode and decode, NetConnection/NetStream command flow, publish and play. **Acc:** publish and play round-trip against a reference server with the same command sequence the reference emits | `vaco-protocol-rtmp` | 18 §8.6 | 4.0 | PR-09a | 4 |
| PR-09c | Tunnelled variants (`rtmpt`, `ffrtmphttp`) and `rtmps` over PR-05. **Acc:** each variant completes a publish and a play round-trip against a reference server | `vaco-protocol-rtmp` | 18 §8.6 | 2.0 | PR-09b, PR-06 | 4 |
| **PR-10** *(group)* | `srt` native, from `draft-sharabayko-srt`. **Blocked on §8 item 8 (yes/no)** — **split into 3 children below; §11.** | `vaco-protocol-srt` | 18 §8.6 | **12.0** | PR-04 | 4 |
| PR-10a | UDT-derived packet framing, handshake (caller/listener/rendezvous), encryption negotiation. **Acc:** all three handshake modes complete against a reference SRT peer; the fuzz target on packet parsing is green for 24 h | `vaco-protocol-srt` | 18 §8.6 | 4.0 | PR-04 | 4 |
| PR-10b | Congestion control, ACK/NAK, retransmission, the latency window and drop policy. **Acc:** a lossy-link simulation at 5% and 20% loss recovers to the same delivered-packet set as the reference peer | `vaco-protocol-srt` | 18 §8.6 | 5.0 | PR-10a | 4 |
| PR-10c | Stream and message transmission modes, the statistics surface, the option surface, interop matrix. **Acc:** the interop matrix against a reference peer passes in both directions at every mode | `vaco-protocol-srt` | 18 §8.6 | 3.0 | PR-10b | 4 |
| **PR-11** *(group)* | **`rist` native (VSF TR-06-1/2)** — **split into 3 children below; §11.** | `vaco-protocol-rist` | 18 §8.7 | **10.0** | PR-04 | 4 |
| PR-11a | Simple profile: RTP/RTCP framing, retransmission requests, the buffer and latency model. **Acc:** a simple-profile session against a reference peer recovers a lossy link to the same delivered set | `vaco-protocol-rist` | 18 §8.7 | 4.0 | PR-04 | 4 |
| PR-11b | Main profile: GRE tunnelling, encryption, authentication. **Acc:** a main-profile encrypted session completes against a reference peer in both directions | `vaco-protocol-rist` | 18 §8.7 | 4.0 | PR-11a | 4 |
| PR-11c | Bonding and multi-link, the statistics surface, interop matrix. **Acc:** a bonded two-link session survives the loss of either link with no delivered-packet loss | `vaco-protocol-rist` | 18 §8.7 | 2.0 | PR-11b | 4 |
| **PR-12** *(group)* | `sctp`, `shared`, `dtls` — **three crates, split for single-writer ownership** — **split into 3 children below; §11.** | `vaco-protocol-*` | 18 §8.7 | **8.0** | PR-04 | 4 |
| PR-12a | `sctp`. **Acc:** a session round-trips against a reference peer and the fuzz target is green for 24 h | `vaco-protocol-sctp` | 18 §8.7 | 3.0 | PR-04 | 4 |
| PR-12b | `dtls` over PR-05's TLS stack. **Acc:** a DTLS session completes against a reference peer under the D14.2 crypto provider | `vaco-protocol-dtls` | 18 §8.7 | 3.0 | PR-05 | 4 |
| PR-12c | `shared`. **Acc:** the shared-memory transport round-trips between two local processes | `vaco-protocol-shared` | 18 §8.7 | 2.0 | PR-04 | 4 |
| | **Subtotal** | | | **142.5** | | |

### 1.15 Containers (`FM-`, `XF-`, plan 18 §8.4–8.7)

| ID | Title | Crate | Origin | pw | Deps | Wave |
|---|---|---|---|---:|---|---|
| FM-01 | MP4 box walk, tracks, `SampleCursor`, packet output, MP4-O1/O2 | `vaco-demux-mp4` | 18 §8.4 | 8.0 | SH-01, SH-05, FW-04 | 3 |
| FM-02 | MP4 edit lists, `ctts`/`cslg`, seek | `vaco-demux-mp4` | 18 §8.4 | 4.0 | FM-01, FW-07 | 3 |
| FM-03 | MP4 fragmented (`moof`/`tfdt`/`trun`/`sidx`/`mfra`) | `vaco-demux-mp4` | 18 §8.4 | 3.0 | FM-01 | 3 |
| FM-04 | MP4 metadata, chapters, cover art, timecode, matrix, colour side data | `vaco-demux-mp4` | 18 §8.4 | 3.0 | FM-01, FW-12, SH-09 | 3 |
| FM-05 | MP4 CENC reporting + decryption; HEIF/AVIF items + `TileGrid` | `vaco-demux-mp4` | 18 §8.4 | 4.0 | FM-01 | 3 |
| FM-06 | EBML reader, schema, unknown-size termination, recovery | `vaco-demux-matroska` | 18 §8.4 | 3.0 | FW-02 | 3 |
| FM-07 | Matroska header, tracks, codec mapping, colour/HDR, clusters, lacing, content encodings | `vaco-demux-matroska` | 18 §8.4 | 5.0 | FM-06, SH-02, SH-09 | 3 |
| FM-08 | Matroska cues/seek, tags, chapters, attachments, delay/preroll/padding, `webm_dash_manifest` | `vaco-demux-matroska` | 18 §8.4 | 3.0 | FM-07, FW-07, FW-12 | 3 |
| FM-09 | MPEG-TS PSI, programs, descriptors, streams | `vaco-demux-mpegts` | 18 §8.4 | 4.0 | SH-03, SH-04 | 3 |
| FM-10 | MPEG-TS PES, timestamps, wrap, continuity/discontinuity, `mpegtsraw`, m2ts | `vaco-demux-mpegts` | 18 §8.4 | 4.0 | FM-09, FW-05 | 3 |
| FM-11 | MPEG-TS seek, duration `FromPts`, PMT-version options | `vaco-demux-mpegts` | 18 §8.4 | 4.0 | FM-10, FW-07, FW-06 | 3 |
| XF-01 | `tests/conformance/probe/{isobmff,matroska,mpegts}.toml` + corpus + **the 26 VERIFY experiments** (P1–P7, T1–T5, S1, M1–M7, K1–K4, A1, N1, L1) | `vaco-conformance` | 18 §8.4 | 6.0 | FM-01..11, X-01, QA-02 | 3 |
| XF-02 | Fuzz targets for core, IO and the three demuxers + corpus minimisation | fuzz | 18 §8.4 | 3.0 | FM-01..11, QA-05 | 3 |
| FM-20 | Utility muxers: crc, framecrc, framemd5, framehash, hash, md5, streamhash, uncodedframecrc, null, mkvtimestamp_v2. **Needed by the harness from v0.1** — pull forward if XF-01 needs it. | `vaco-mux-utility` | 18 §8.5 | 2.0 | FW-08 | 3 |
| FM-21 | MP4 mux: tables, chunked interleave, trailer, faststart | `vaco-mux-mp4` | 18 §8.5 | 7.0 | FW-09, SH-01 | 4 |
| FM-22 | MP4 mux: fragmented, all `movflags`, CENC write, profile variants | `vaco-mux-mp4` | 18 §8.5 | 5.0 | FM-21 | 4 |
| FM-23 | MP4 mux: metadata write, `avif`/`ipod`/`ismv`/`f4v`/`psp`/`3gp`/`3g2` | `vaco-mux-mp4` | 18 §8.5 | 4.0 | FM-21, FW-12 | 4 |
| FM-24 | Matroska mux (+ webm, matroska_audio, webm_chunk) | `vaco-mux-matroska` | 18 §8.5 | 8.0 | FW-09, FM-06 | 4 |
| FM-25 | MPEG-TS mux | `vaco-mux-mpegts` | 18 §8.5 | 7.0 | FW-09, SH-03 | 4 |
| **FM-26** *(group)* | Raw demux + mux (48 + 40 registrations) — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-raw`, `vaco-mux-raw` | 18 §8.5 | **6.0** | FW-04, FW-09 | 4 |
| FM-26a | Raw demux: all 48 registrations (PCM, rawvideo, bitstream families), probing and stream discovery. **Acc:** every registration probes and demuxes to the same packet stream the reference produces | `vaco-demux-raw` | 18 §8.5 | 3.5 | FW-04 | 4 |
| FM-26b | Raw mux: all 40 registrations. **Acc:** every registration produces byte-identical output to the reference over the corpus | `vaco-mux-raw` | 18 §8.5 | 2.5 | FW-09 | 4 |
| FM-27 | wav, w64, aiff, caf, au, voc, sox, ircam, rso — demux + mux | `vaco-format-audio-simple` | 18 §8.5 | 7.0 | SH-02, SH-06, SH-08 | 4 |
| **FM-28** *(group)* | AVI demux + mux — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-avi`, `vaco-mux-avi` | 18 §8.5 | **6.0** | SH-02 | 4 |
| FM-28a | AVI demux: RIFF walk, index (idx1/OpenDML), streams, seek, non-interleaved handling. **Acc:** the AVI corpus demuxes to the same packet stream and seek results as the reference | `vaco-demux-avi` | 18 §8.5 | 3.5 | SH-02, FW-07 | 4 |
| FM-28b | AVI mux: index generation, OpenDML, interleaving. **Acc:** remuxed AVI bytes are identical to the reference with `-fflags +bitexact` | `vaco-mux-avi` | 18 §8.5 | 2.5 | FW-09, SH-02 | 4 |
| **FM-29** *(group)* | FLV demux + mux — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-flv`, `vaco-mux-flv` | 18 §8.5 | **5.0** | SH-05 | 4 |
| FM-29a | FLV demux: tag walk, AMF metadata, codec mapping, seek. **Acc:** the FLV corpus demuxes to the same packet stream and metadata the reference reports | `vaco-demux-flv` | 18 §8.5 | 3.0 | SH-05 | 4 |
| FM-29b | FLV mux incl. the `f4v`-adjacent options and AMF metadata write. **Acc:** remuxed FLV bytes are identical to the reference with `-fflags +bitexact` | `vaco-mux-flv` | 18 §8.5 | 2.0 | FW-09, SH-05 | 4 |
| **FM-30** *(group)* | Ogg demux + mux (+ oga/ogv/opus/spx) — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-ogg`, `vaco-mux-ogg` | 18 §8.5 | **8.0** | SH-07 | 4 |
| FM-30a | Ogg demux: page/packet layer, all codec mappings, granulepos→timestamp rules, seek. **Acc:** the Ogg corpus demuxes to the same packet stream and timestamps the reference reports, including chained streams | `vaco-demux-ogg` | 18 §8.5 | 4.5 | SH-07, FW-07 | 4 |
| FM-30b | Ogg mux (+ oga/ogv/opus/spx variants): page packing, granulepos generation. **Acc:** remuxed Ogg bytes are identical to the reference with `-fflags +bitexact` | `vaco-mux-ogg` | 18 §8.5 | 3.5 | FW-09, SH-07 | 4 |
| **FM-31** *(group)* | ASF demux + mux — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-asf`, `vaco-mux-asf` | 18 §8.5 | **7.0** | SH-02 | 4 |
| FM-31a | ASF demux: object model, payload parsing, index, seek, DRM detection and reporting. **Acc:** the ASF corpus demuxes to the same packet stream and seek results as the reference | `vaco-demux-asf` | 18 §8.5 | 4.0 | SH-02, FW-07 | 4 |
| FM-31b | ASF mux: object generation, index write, packetisation. **Acc:** remuxed ASF bytes are identical to the reference with `-fflags +bitexact` | `vaco-mux-asf` | 18 §8.5 | 3.0 | FW-09, SH-02 | 4 |
| **FM-32** *(group)* | MPEG-PS demux + mux family — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-mpegps`, `vaco-mux-mpegps` | 18 §8.5 | **9.0** | SH-04 | 4 |
| FM-32a | MPEG-PS demux: pack and system headers, PES over SH-04, stream discovery, private-stream mapping, seek. **Acc:** the MPEG-PS corpus demuxes to the same packet stream, stream set and seek results as the reference | `vaco-demux-mpegps` | 18 §8.5 | 5.0 | SH-04, FW-07 | 4 |
| FM-32b | MPEG-PS mux family (mpeg1system/vcd/dvd/svcd/vob): muxing model, SCR/PCR, padding, VOB specifics. **Acc:** remuxed bytes are identical to the reference for every variant with `-fflags +bitexact` | `vaco-mux-mpegps` | 18 §8.5 | 4.0 | FW-09, SH-04 | 4 |
| FM-33 | concat, ffmetadata, segment, stream_segment, tee, fifo | `vaco-format-meta` | 18 §8.5 | 6.0 | FW-09, IO-03 | 4 |
| FM-34 | Text subtitle formats (15 demux / 6 mux) | `vaco-format-subtitle-text` | 18 §8.5 | 6.0 | FW-02 | 4 |
| **FM-35** *(group)* | image2 demux + mux incl. the 42 pipe splitters — **two crates, split for single-writer ownership** — **split into 2 children below; §11.** | `vaco-demux-image2`, `vaco-mux-image2` | 18 §8.5 | **4.0** | FW-04 | 4 |
| FM-35a | image2 demux, glob/sequence patterns, and all 42 pipe splitters. **Acc:** every splitter segments its corpus identically to the reference; sequence and glob patterns resolve identically | `vaco-demux-image2` | 18 §8.5 | 2.5 | FW-04 | 4 |
| FM-35b | image2 mux: filename patterns, `-update`, `strftime`, atomic write. **Acc:** output filenames and bytes are identical to the reference over the corpus | `vaco-mux-image2` | 18 §8.5 | 1.5 | FW-09 | 4 |
| FM-36 | NUT | `vaco-format-nut` | 18 §8.5 | 5.0 | FW-09 | 4 |
| FM-37 | DV container | `vaco-format-dv` | 18 §8.5 | 3.0 | SH-02 | 4 |
| FM-38 | MPJPEG | `vaco-format-mpjpeg` | 18 §8.5 | 1.0 | FW-04 | 4 |
| FM-40 | RTSP/SDP session layer + transport modes | `vaco-format-rtp` | 18 §8.6 | 8.0 | PR-08 | 4 |
| FM-41 | 26 implementable RTP depacketisers | `vaco-format-rtp` | 18 §8.6 | 8.0 | FM-40, SH-05 | 4 |
| FM-42 | RTP packetisers + `rtp_mpegts` | `vaco-format-rtp` | 18 §8.6 | 6.0 | FM-41 | 4 |
| FM-43 | HLS demux | `vaco-demux-hls` | 18 §8.6 | 8.0 | PR-06, FM-03, FM-25, SH-06 | 4 |
| FM-44 | HLS mux | `vaco-mux-hls` | 18 §8.6 | 8.0 | FM-33, FM-22 | 4 |
| FM-45 | DASH demux (`quick-xml`) | `vaco-demux-dash` | 18 §8.6 | 8.0 | PR-06, FM-03 | 4 |
| FM-46 | DASH mux | `vaco-mux-dash` | 18 §8.6 | 8.0 | FM-33, FM-22 | 4 |
| **FM-50** *(group)* | **MXF demux** — **split into 4 children below; §11.** | `vaco-demux-mxf` | 18 §8.7 | **14.0** | SH-01, FW-07 | 4 |
| FM-50a | KLV layer, partition and RIP discovery, header-metadata primer, the crate's internal model and interface freeze. **Acc:** every file in the MXF corpus enumerates the same partition and KLV structure the reference reports | `vaco-demux-mxf` | 18 §8.7 | 4.0 | SH-01 | 4 |
| FM-50b | Structural metadata graph: packages, tracks, sequences, source clips, descriptors, and their mapping onto `Stream`. **Acc:** the stream set, codec parameters and metadata match the reference for every corpus file | `vaco-demux-mxf` | 18 §8.7 | 4.0 | FM-50a | 4 |
| FM-50c | Essence containers: frame- and clip-wrapped mappings, D-10 mapping, generic containers, sound item mapping. **Acc:** the demuxed packet stream matches the reference for every essence mapping in the corpus | `vaco-demux-mxf` | 18 §8.7 | 3.0 | FM-50b | 4 |
| FM-50d | Index tables (all CBE and VBE forms), seek, OP-Atom and OP1a specifics, timecode tracks. **Acc:** seek results and timecode reporting match the reference for every corpus file | `vaco-demux-mxf` | 18 §8.7 | 3.0 | FM-50c, FW-07 | 4 |
| **FM-51** *(group)* | **MXF mux (+ d10, opatom)** — **split into 3 children below; §11.** | `vaco-mux-mxf` | 18 §8.7 | **12.0** | FM-50 | 4 |
| FM-51a | KLV writer, partition layout, header-metadata generation, primer pack. **Acc:** generated headers parse identically under FM-50 and under the reference demuxer | `vaco-mux-mxf` | 18 §8.7 | 4.0 | FM-50b | 4 |
| FM-51b | Essence wrapping and index-table generation for OP1a. **Acc:** remuxed OP1a bytes are identical to the reference with `-fflags +bitexact` | `vaco-mux-mxf` | 18 §8.7 | 4.0 | FM-51a | 4 |
| FM-51c | `d10` (SMPTE 386M) and `opatom` variants, timecode track, the remux byte-identity matrix. **Acc:** remuxed bytes are identical to the reference for all three variants across the corpus | `vaco-mux-mxf` | 18 §8.7 | 4.0 | FM-51b, FM-50d | 4 |
| FM-52 | Bitmap subtitles (dvbsub, dvbtxt, sup/PGS, vobsub) | `vaco-format-subtitle-bitmap` | 18 §8.7 | 6.0 | SH-04 | 4 |
| FM-53 | IAMF | `vaco-format-iamf` | 18 §8.7 | 7.0 | SH-01 | 4 |
| FM-54 | S/PDIF + `s337m` | `vaco-format-spdif` | 18 §8.7 | 2.5 | SH-04 | 4 |
| **FM-55** *(group)* | GXF + IMF — **two crates, split for single-writer ownership** — **split into 3 children below; §11.** | `vaco-format-gxf`, `-imf` | 18 §8.7 | **10.0** | SH-01 | 4 |
| FM-55a | GXF demux + mux. **Acc:** the GXF corpus demuxes identically to the reference and remuxes byte-identically | `vaco-format-gxf` | 18 §8.7 | 4.0 | FW-04, FW-09 | 4 |
| FM-55b | IMF: CPL, PKL and ASSETMAP parsing, virtual-track assembly. **Acc:** the IMF corpus resolves to the same virtual-track timeline the reference reports | `vaco-format-imf` | 18 §8.7 | 4.0 | SH-01 | 4 |
| FM-55c | IMF essence integration over the MXF demuxer, plus conformance. **Acc:** IMF packages demux to the same packet stream the reference produces | `vaco-format-imf` | 18 §8.7 | 2.0 | FM-55b, FM-50d | 4 |
| FM-56 | SWF | `vaco-format-swf` | 18 §8.7 | 4.0 | SH-02 | 4 |
| **FM-57** *(group)* | Smooth Streaming, HDS, WHIP muxers — **three crates, split for single-writer ownership** — **split into 3 children below; §11.** | `vaco-mux-smoothstreaming` etc. | 18 §8.7 | **8.0** | FM-22, PR-06 | 4 |
| FM-57a | Smooth Streaming mux. **Acc:** the manifest and fragment set match the reference's structure and the output plays back through a reference client | `vaco-mux-smoothstreaming` | 18 §8.7 | 3.0 | FM-22, PR-06 | 4 |
| FM-57b | HDS mux. **Acc:** the manifest and fragment set match the reference's structure | `vaco-mux-hds` | 18 §8.7 | 2.0 | FM-22 | 4 |
| FM-57c | WHIP mux. **Acc:** a WHIP session negotiates and publishes against a reference endpoint | `vaco-mux-whip` | 18 §8.7 | 3.0 | PR-06, FM-42 | 4 |
| **FM-58** *(group)* | **T3 audio containers** — **split into 3 children below; §11.** | `vaco-format-misc-audio` | 18 §8.7 | **10.0** | FW-04, SH-08 | 4 |
| FM-58a | Tracker/module and chiptune-adjacent audio containers. **Acc:** every format in the group demuxes to the same stream set and packet stream the reference produces | `vaco-format-misc-audio` | 18 §8.7 | 3.0 | FW-04 | 4 |
| FM-58b | Legacy compressed-audio containers (APE, TTA, WavPack, Musepack framing). **Acc:** every format in the group demuxes identically to the reference, with tags via SH-08 | `vaco-format-misc-audio` | 18 §8.7 | 4.0 | FW-04, SH-08 | 4 |
| FM-58c | Remaining T3 audio containers. **Acc:** every format in the group demuxes identically to the reference | `vaco-format-misc-audio` | 18 §8.7 | 3.0 | FM-58b | 4 |
| **FM-59** *(group)* | **T3 remainder** — **split into 3 children below; §11.** | `vaco-format-misc` | 18 §8.7 | **10.0** | FW-04 | 4 |
| FM-59a | T3 video containers, first group. **Acc:** every format in the group demuxes identically to the reference | `vaco-format-misc` | 18 §8.7 | 4.0 | FW-04 | 4 |
| FM-59b | T3 video containers, second group. **Acc:** every format in the group demuxes identically to the reference | `vaco-format-misc` | 18 §8.7 | 3.0 | FM-59a | 4 |
| FM-59c | T3 miscellaneous and metadata containers. **Acc:** every format in the group demuxes identically to the reference | `vaco-format-misc` | 18 §8.7 | 3.0 | FM-59a | 4 |
| XF-03 | Format-wide conformance expansion: the remux byte-identity matrix across every T1 muxer | `vaco-conformance` | 18 §8.7 | 6.0 | FM-21..FM-32 | 4 |
| XF-04 | The differential fuzzer for formats: mutate real media, feed both, assert agreement | fuzz | 18 §8.7 | 4.0 | XF-02 | 4 |
| XF-05 | `docs/formats/*` completeness + **`docs/why-some-formats-are-not-included.md`** — a v1.0 deliverable, not an afterthought | docs | 18 §8.7 | 4.0 | all FM | 5 |
| | **Subtotal** | | | **311.5** | | |

### 1.16 Register roll-up

| Group | Prefix | Net pw | Plan's stated pw |
|---|---|---:|---:|
| Phase 0 contract | `P0-` | 5.0 | *(not packaged)* |
| Foundations | `FD-` | 24.0 | 27.0 |
| Performance | `PF-` | 66.0 | 168.5 |
| Correctness | `QA-` | 33.5 | *(none stated)* |
| CLI | `CL-` | 75.0 | 77.0 |
| Codec framework + harness | `F-`, `X-` | 23.0 | *(inside 383)* |
| Shared codec DSP / entropy / CBS | `D-` | 98.0 | *(inside 383)* |
| v0.1 parsers | `P-` | 19.0 | *(inside 383)* |
| T1 codecs | `C-` | 288.5 | *(inside 383)* |
| BSF + T2 + hardware, unconditional | `B-`, `T2-`, `H-01/02/06/07` | 219.0 | *(beyond 383)* |
| T3 in-tree, in v1.0 | `T3-01..03` | 145.0 | *(beyond 383)* |
| Filters | `FT-` | 224.0 | 228.0 |
| Signal processing, excl. deferred SP-C13 | `SP-` | 108.0 | 113.0 |
| I/O, protocols, format framework | `IO-`,`FW-`,`SH-`,`PR-` | 142.5 | *(inside 455.5)* |
| Containers | `FM-`, `XF-` | 311.5 | *(inside 455.5)* |
| **Register total, full v1.0 scope** | | **1,782.0** | |

Three scope reductions this document recommends, each argued where it appears:

| Reduction | Δ | Why |
|---|---:|---|
| Conditional hardware backends H-03 (VA-API), H-04 (D3D12), H-05 (NVDEC/NVENC) | −33.0 | D13's table says Vulkan Video covers Linux/Windows/Android and that VA-API and NVDEC are "only if Vulkan Video proves insufficient. Prefer not to." Plan 15 §7.5 schedules all five as first-class. D13 governs. See §9.6. |
| Filter T3 long tail FT-4.12 / FT-4.13 (~214 filters) | −48.0 | Plan 16 itself calls this "implemented opportunistically". It is the most parallelisable work in the project and the natural post-1.0 contributor on-ramp. It does not gate v1.0. |
| Format T3 remainder FM-58 / FM-59 | −20.0 | Plan 18 tiers these T3. Same argument. |
| **Planning case for v1.0** | | **1,681.0 ≈ 1,680 pw** |

Post-1.0 backlog, explicitly out of the v1.0 number: T3-04 (AAC encode 25), T3-05 (AC-3 encode 12),
T3-06 (DTS core 40), T3-07 (VVC 110 — **plan 15 recommends never**), T4-\* (~250), T5-01 (~300),
SP-C13 (2), plus the three reductions above if they are wanted (101). **~840 pw.**

---

## 2. Reconciling the totals

### 2.1 The naive sum

| Plan | Domain | Stated total (pw) |
|---|---|---:|
| 11 | Foundations | 27.0 |
| 12 | Performance | 168.5 |
| 13 | Correctness | **none stated** |
| 14 | CLI | 77.0 |
| 15 | Codecs (**T1 only**) | 383.0 |
| 16 | Filters | 228.0 |
| 17 | Scale / resample / tx | 113.0 |
| 18 | Formats | 455.5 |
| | **Naive sum** | **1,452.0** |

That number is wrong in five directions at once, and only one of them makes it smaller.

### 2.2 The bridge

| Step | Δ | Running |
|---|---:|---:|
| Naive sum of the seven stated headlines | | 1,452.0 |
| **Plan 13 carries no estimate at all.** Its `vaco-limits`, `vaco-conformance`, `vaco-corpus`, `vaco-fuzz-support`, `xtask`, CI, provenance and release-engineering scope is real work that no other plan budgets. Estimated here as QA-01…QA-10. | **+33.5** | 1,485.5 |
| **Plan 19's Phase 0 is described but never packaged.** "2–3 weeks of serial work" for ~120 crate skeletons and a full interface freeze. | **+5.0** | 1,490.5 |
| **Plan 16 §8.7's cumulative table double-counts** items 3.5 (`vaco-filter-text`, 4.0) and 4.10 (drawtext family, 4.0) — one of the two appears in both "Shared helpers" and "Text + subtitles". Its own phase subtotals sum to 224.0, not 228.0. | −4.0 | 1,486.5 |
| **Plan 12's Phase 0 arithmetic.** The items sum to 19.5; the section header says 19 and the summary table says 20. The plan total is 168.0, not 168.5. | −0.5 | 1,486.0 |
| **Plan 15's §4.15 roll-up (383) is a narrower scope than its own §7 package tables (439.5 for waves 0–2).** §4.15 omits the conformance harness (X-01…X-06, 15), the T2-only DSP crates (D-15 dwt 4, D-22 mpegvideo 14), the encoder-side DSP (D-13 me 5, D-14 ratecontrol 5), the CBS write path (D-19, 6) and `vaco-codec-exec` (C-46, 4). **§7 is the executable plan; §4.15 is a summary of a subset. §7 governs.** | **+56.5** | 1,542.5 |
| Remove double counting (§2.3) | **−124.5** | **1,418.0** |

**Deduplicated total at plan 15's T1-codec scope: 1,418 pw ≈ 27 person-years.**

That is not v1.0. Adding the packages plan 15 §9.1 and plan 18 §8.7 place inside v1.0 — bitstream
filters, T2 codecs, the unconditional hardware backends, the three in-tree T3 decoders, the muxers,
protocols and second-tier containers — gives the register total of **1,782 pw**, and the planning
case after §1.16's three recommended scope reductions is **~1,680 pw ≈ 32 person-years**.

### 2.3 The double counting, named and quantified

Every one of these is the same work claimed by two or three plans. The rule used to adjudicate is:
**the crate's single writer owns the estimate.** Under plan 19 §2, an agent assigned `vaco-scale`
writes `crates/dsp/vaco-scale/**` and nothing else — so plan 12 *cannot* be the writer of a SIMD
kernel that lives inside `vaco-scale`. Plan 12 therefore owns the kernel-authoring standard, the
verification tooling and the measurement programme; the domain plans own the kernels.

| # | Overlap | Claimed by | Adjudication | Δ |
|---:|---|---|---|---:|
| 1 | **`vaco-simd` substrate** | plan 11 (3.0), plan 12 PF-0.0+0.1 (3.0), plan 17 D.1.1 (3.0) — **9.0 for one crate** | Plan 12 owns it: it is the only one that itemises the D12 gap compositions and the adoption checklist. Plan 17's prerequisite line and plan 11's `vaco-simd` row are struck. Plan 11's bitstream `scan` idioms fold into PF-0.1 (+0.5). | −5.5 |
| 2 | **`vaco-checkasm`** | plan 12 PF-0.2+0.3 (5.0), plan 15 X-02 (3.0), plan 17 D.1.1 (2.0) — **10.0 for one crate** | Plan 12 owns it. Its estimate is the only one that separates verify mode from bench mode and specifies the `perf-event` backend and the nop-baseline protocol. | −5.0 |
| 3 | **`vaco-tx`** | plan 15 D-16 (8.0), plan 17 Part C (27.0), plan 12 PF-3.4 (8.0 for its SIMD alone) | Plan 17 owns it. It is the only plan that specifies the **bit-exact i32 fixed-point arithmetic contract** that D10 says several audio codecs require for conformance, and `docs/tx/fixed-point.md` is normative for codecs. **Plan 15 under-costed `vaco-tx` by 3.4×.** | −16.0 |
| 4 | **`vaco-scale` and `vaco-resample` SIMD kernels** | plan 12 Phase 1 (22.5: colour conversion, packed↔planar, swscale h/v filters, audio format conversion + rematrix, polyphase, dither), plan 17 SP-A9/A10/B3/B6/B9 (15.0) | Plan 17 owns them — the crate's writer writes them. Plan 12's Phase 1 is struck and its kernel inventory folded into SP-A9/A10 as scope, not as extra weeks. | −22.5 |
| 5 | **Filter SIMD kernels** | plan 12 Phase 2 (25.0: deinterlace, blur, denoise, blend, analysis, lut3d/tonemap, audio filters), plan 16 items 4.4–4.11 (50.0) | Split. Plan 16's per-filter figures are lean and do not obviously carry plan 12's kernel-authoring standard (scalar reference + checkasm registration + divan bench per kernel). Half the overlap is real incremental work; half is already inside plan 16's numbers. **PF-2.x kept at 12.5, down from 25.** | −12.5 |
| 6 | **Codec DSP kernels** | plan 12 Phase 3 excl. `vaco-tx` (77.0), plan 15 `D-05`…`D-14` + the C-/T3- codec packages | Mostly plan 15's. Plan 15's definition of done explicitly reads "a scalar reference and `vaco-checkasm` differential test for every kernel + criterion bench", so its D- and C- estimates already include kernel authoring. What is genuinely incremental is the **batched-dispatch redesign** (PF-3.2, which changes the `Decoder`↔`KernelSet` contract and must land before F-02 freezes), cross-codec tuning, and the §1.3 band-verification programme. **PF-3.x kept at 23.0, down from 85.** | −62.0 |
| 7 | **Scalar / entropy hot paths** | plan 12 Phase 4 (16.0), plan 11 `vaco-bitstream` (2.5), plan 15 D-01/D-02/D-03, plan 18's demuxers | The bit-reader design (PF-4.1) is inside FD-03; CABAC/CAVLC table layout is inside D-03/D-01; the demux hot paths are inside FM-01/07/10. PGO/BOLT tuning (PF-4.4), startup latency (PF-4.6) and the residual are genuinely plan 12's. **Phase 4 kept at 10.5.** | −5.5 |
| 8 | **NALU handling** | plan 18 SH-05 `vaco-format-nalu` (3.0), plan 11 FD-03 start-code scanning (2.5), plan 15 P-01 (3.0) | Genuinely three different things — config records and Annex-B↔length-prefix conversion (SH-05), the scanning primitive (FD-03), SPS/PPS semantic parsing (P-01) — but the scanning half is shared. | −1.5 |
| 9 | **Probe conformance matrix** | plan 14 CL-10 (3.0), plan 18 XF-01 (6.0) | The same ~9,000-invocation matrix over the same three corpora. Plan 18 owns the corpus and the 26 VERIFY experiments; plan 14 owns the writer-side assertions. | −2.0 |
| | **Total double counting removed** | | | **−124.5** |

Two overlaps are **deliberately not deducted**, and it is worth saying why:

- **Plan 15's `X-01`…`X-06` versus plan 13's harness scope.** These would double-count if plan 13 had
  estimates. It does not, so `X-01` (comparator core) and `X-03` (fuzz scaffolding) are treated as
  plan 13's first two packages under plan 15's IDs, and `QA-02`…`QA-05` are scoped as *beyond* them.
  Same treatment for plan 18's `XF-0n` and plan 16's `FT-6.x`.
- **Plan 16's `scale` filter (inside FT-4.1) versus plan 17's `vaco-scale` crate (SP-A\*).** No overlap:
  the filter is a thin `Simple`-adapter wrapper over the crate. Flagged so nobody budgets it twice
  later.

### 2.4 The calendar, under the 4–8 agent ceiling

Plan 19 §7 is unambiguous: "Ready" and "at once" are different columns, and on a 10-core / 16 GB
machine the measured sustainable figure is 4–6 agents, reaching 8 only in wave 4 under the full
protocol. Each agent runs `-j 4`, so six agents already oversubscribe ten cores and **memory, not
CPU, binds first**.

An agent-week is not a person-week. Wave boundaries, integration runs, escalations that stop an
agent mid-task, and conformance-triage rework put effective utilisation at **0.75–0.8**.

| Sustained agents | Effective pw/week | Weeks to 1,680 pw | Calendar |
|---:|---:|---:|---|
| 4 | 3.2 | 525 | **10.1 years** |
| 6 | 4.5 | 373 | **7.2 years** |
| 8 | 6.0 | 280 | **5.4 years** |
| Blended (4–6 through waves 1–3, 6–8 in wave 4, 3–4 in wave 5) | ~5.0 | 336 | **6.5 years** |

**The blended row is the planning case: ~336 weeks, ~6.5 years to v1.0.**

Three things follow, and they are the most useful conclusions in this document:

1. **The project is throughput-bound by a factor of six.** The dependency-critical path is 53.5 weeks
   (§3, recomputed after the §11 decomposition pass). The calendar is 336. The architecture has already succeeded at its stated job — plan 19 §7's
   promise that "there is *always* an unblocked crate to hand the next free agent" holds — and the
   remaining lever is machine capacity, not design.
2. **Doubling the hardware roughly halves the calendar, up to about 8–10 agents.** This is the single
   highest-leverage decision available and it is a purchasing decision, not an engineering one. It is
   §8 item 2.
3. **The useful product arrives long before v1.0.** v0.3 (§4.3) is a working transcoder at ~730 pw
   cumulative — **43% of the effort for something people can actually use.** Scope discipline after
   v0.3 is worth more than any acceleration before it.

A roadmap that implied otherwise would be worthless, so: **this is a six-to-seven-year project at
realistic concurrency, and no amount of planning changes that.** What planning changes is the order,
and the order is what decides whether year two produces something usable or a half-finished H.264
decoder nobody is allowed to ship.

---

## 3. The critical path

### 3.1 What is genuinely serial

Four serialisations are real and are confirmed by more than one plan:

1. **Phase 0 contract-first** (plan 19 §6). Nothing parallel is safe until every public interface
   exists and compiles. 3 weeks, 1–2 agents, and it "converts ~120 crates from a dependency-ordered
   chain into a set of independent tasks". Every plan's parallelism claim is downstream of it.
2. **`vaco-codec-core` before `vaco-format-core`** (D14.1). Plan 18 §1.0 established that demuxers
   need codec parameters, so F-01 gates FW-01 gates all 40 format crates. This is the longest single
   serial prefix in the project and plan 15 is right that "getting `vaco-codec-core` right is worth
   more than any other six weeks".
3. **`vaco-tx` before the transform-coded audio codecs.** SP-C6 (derived transforms: MDCT/IMDCT,
   RDFT, DCT-II/III) gates Vorbis, Opus CELT, MP3, AC-3, AAC and DTS. Plan 17 D.1.2 is right that
   `vaco-tx` should start first among its three crates for exactly this reason.
4. **`vaco-filter-core` before the filters.** FT-1.7's adapters are written against by ~560 filters.
   21.5 pw gates ~190 pw.

### 3.2 What only looks serial

- **The 214-filter T3 long tail** (FT-4.12/4.13) looks like 48 pw of work; it is 214 independent
  1.5-day tasks behind a single dependency (FT-1.7). It is the most parallel work in the project.
- **The ~100 T4 codec and format crates** are the same shape.
- **Wave 4 as a whole.** Plan 19 §7 calls it "almost perfectly parallel *logically*". It is. The
  constraint is machine capacity, not order.
- **AV1** looks like one 70 pw monolith. Plan 15 split it into eleven packages (C-34…C-44) against an
  interface frozen up front precisely so five people can work on it at once. That split is why AV1 is
  *on* the critical path rather than *being* it.
- **`vaco-scale`'s 48.5 pw** looks like a long pole. Its own critical path is 23 pw
  (SP-A1→A2→A3→A8→A9→A10); the rest runs in three parallel lanes.

### 3.3 The chain that determines the end date

**How this section was produced.** After the §11 decomposition pass the chains below were recomputed
**mechanically from the register's own `Deps` column**, not drawn by hand — every package's earliest
finish is `max(finish of its dependencies) + its own pw`. Two of the hand-drawn chains in the previous
revision did not survive that check and are corrected in §3.3.1.

**The critical path is now 53.5 weeks, and it runs through HEVC.**

**Chain ε — HEVC decode (53.5 weeks). The critical path.**

```
P0-01 (1) → P0-02 (1) → P0-03 (1.5)          [3.5  contract]
  → FD-01 vaco-core (2)                      [5.5]
  → FD-02 (1) → FD-12 vaco-packet (1)        [7.5]
  → F-01  vaco-codec-core identity (3)       [10.5]
  → D-17  vaco-cbs-core (3)                  [13.5]
  → D-18  CBS h2645 read (6)                 [19.5]
  → P-01  H.264/HEVC header subset (3)       [22.5]
  → T3-02b parameter sets (3)                [25.5]
  → T3-02c slice segment header + RPS (4)    [29.5]
  → T3-02d CABAC contexts + WPP sync (7)     [36.5]
  → T3-02e CTU quadtree (5)                  [41.5]
  → T3-02f intra prediction (4)              [45.5]
  → T3-02m threading + RExt + integration(5) [50.5]
  → T3-02n JCT-VC conformance (3)            [53.5]
```

**Chain δ — H.264 decode (52.5 weeks).** Same prefix through D-18, then
`T3-01a (4) → T3-01b (3) → T3-01c (3) → T3-01e CABAC (6) → T3-01f MB layer (5) →
T3-01g intra (5) → T3-01m integration (4) → T3-01n conformance (3)`.

**Chain ζ — the streaming tail and its documentation (52.5 weeks).** Not previously identified as a
long chain, and it is one:
`P0 (3.5) → FD-01 (2) → QA-01 (1.5) → IO-01 (4) → IO-03 (3) → PR-04 (4.5) → PR-08 rtp (5) →
FM-40 RTSP/SDP (8) → FM-41 depacketisers (8) → FM-42 packetisers (6) → FM-57c WHIP (3) →
XF-05 the formats-omission document (4)`. XF-05 is gated on *every* `FM-` package, so the last
container to land sets its start date.

**Chain α — the AV1 decoder (51.5 weeks).**

```
P0 (3.5) → FD-01 (2) → FD-02+FD-12 (2) → F-01 (3) → D-17 (3) → D-20 (4)
  → C-34 OBU layer (5) → C-35 frame header, ref mgmt, tiles (8)
  → C-37 tile/superblock loop (5) → C-41b CDEF (2) → C-41d loop restoration (2)
  → C-42 film grain (4) → C-43 threading + DPB (5) → C-44 Argon triage (3)   [51.5]
```

**Chain η — VP9 decode → encode (49.5 weeks).** `… F-01 (3) → D-04 (3) → C-29 (5) → C-30 (7) →
C-32a (3) → C-32c (3) → C-33a (4) → C-33b (4) → C-33c (5) → C-33e (3) → C-33f (2)`. **This chain was
not in the previous revision at all and it was 59.5 weeks before the pass** — the longest in the
register, longer than the 55 the headline claimed. §11.3 records what shortened it.

**Chain β — MXF (39.5 weeks).** `… FW-01 (3) → FW-02 (2) → SH-01 (4) → FM-50a (4) → FM-50b (4) →
FM-51a (4) → FM-51b (4) → FM-51c (4)`. The previous revision put this at 55.5 by serialising the whole
format framework in front of it. The register's `Deps` column does not say that: MXF's KLV and
structural-metadata layers depend on `SH-01`, and only the index/seek child (`FM-50d`) depends on
`FW-07`. Splitting FM-50 is what made that visible.

**Chain γ — the transcode path (45.5 weeks, or 37.0 by the register's own deps).** The previous
revision drew it as `SP-A1…A10 → FT-4.1 → CL-20 → CL-21 → CL-24` and reported 52.5, but its own listed
items sum to 48.5 — an arithmetic slip. Recomputed on the register, `CL-20`'s dependencies are
`CL-19` and `FT-2.6`, which puts the transcode chain at **37.0 weeks**; the narrative reading, in which
`CL-20` waits for the complete `vaco-scale` SIMD programme and the T1 video filters, gives **45.5**.
Either way it is no longer near the top.

**The v0.1 chain (44.5 weeks)** — the one that matters first, and unchanged by this pass:

```
P0 (3.5) → FD-01 (2) → FD-02+FD-12 (2) → F-01 (3) → FW-01 (3) → FW-02 (2)
  → FW-03 (3) → FW-04 (5) → FM-01 vaco-demux-mp4 core (8)
  → FM-02 edit lists + seek (4) → XF-01 conformance + 26 VERIFY experiments (6)
  → v0.1 gate and triage (3)                                        [44.5]
```

#### 3.3.1 Two corrections the recomputation forced

| Claim in the previous revision | What the register's `Deps` column actually gives | Why it mattered |
|---|---|---|
| "Two chains tie at ≈55 weeks" (AV1 and MXF) | AV1 55.5 ✓, MXF 55.5 only if FM-50 waits on `FW-07` in its entirety — 39.5 once the demuxer is split | MXF was never a long pole; the seek dependency belongs to one child, not to all of it. |
| Chain γ "52.5" | Its own listed items sum to 48.5; the register's deps give 37.0 | An arithmetic slip plus a narrative dependency the register does not carry. |
| *(not stated at all)* | **VP9 decode → encode was 59.5 weeks — longer than the stated critical path** | The previous revision's chain set omitted it. It was the real critical path and nobody had noticed, because C-33 was a 22 pw block behind a 9 pw block behind a 7 pw block. |


### 3.4 T3-01 and T3-02: resolved

**This is done.** T3-01 (H.264, 60 pw) and T3-02 (HEVC, 55 pw) were the largest unsplit packages in
the register. Left whole, chain δ was:

```
P0 (3.5) → FD-01 (2) → F-01 (3) → F-02 (2) → D-17 (3) → D-18 CBS h2645 read (6)
  → T3-01 H.264 decode (60)                                          [79.5]
```

**79.5 weeks — 24 weeks longer than everything else, for two decoders we are never allowed to ship.**
§11.1 splits both along the specifications' own structure: bitstream/NAL layer, parameter sets, slice
header, CAVLC and CABAC as separate packages, macroblock layer, intra, inter split into MV derivation
and sample interpolation, transform/dequant, deblocking (and SAO for HEVC), reference-picture
management and the DPB, threading and profile extensions, then conformance. Fourteen children each,
summing exactly to 60.0 and 55.0.

**Result: 79.5 → 52.5 (H.264) and 53.5 (HEVC).** They are no longer twenty-four weeks clear of
everything else; HEVC now sets the end date by one week over H.264 and one week over the streaming
tail, which is what a well-decomposed register looks like.


### 3.5 The honest conclusion about the critical path

**The critical path is ~53.5 calendar weeks. The project is ~336. The critical path accounts for 16%
of the calendar and it is not what determines the end date.**

That was true at 55 weeks and it is still true at 53.5. The decomposition pass in §11 did not buy
calendar — it bought **verifiability and dispatchability**: 544 packages that a single agent can own,
finish and prove inside two months, instead of 400 that included a 60 pw block nobody could report
progress on for a year. The schedule benefit is real but secondary (79.5 → 53.5 on the worst chain);
the execution benefit is the point.

What buys time, in order:

1. More concurrent agents (up to the 8–10 memory ceiling) — roughly linear.
2. Scope reduction after v0.3 — the §1.16 reductions alone are 101 pw, and the post-1.0 backlog is 840.
3. **Splitting the remaining large packages — now done (§11).** It removed 26 weeks from the worst
   chain and, more usefully, removed every package that could fail silently for two months.
4. Crashing the AV1 or HEVC chain — worth ~2 weeks each. Not worth doing.

**Four chains now sit within two weeks of each other** (HEVC 53.5, H.264 52.5, the streaming tail 52.5,
AV1 51.5). That is the signature of a register with no remaining long pole, and it means the next
useful lever is unambiguously throughput, not decomposition.


## 4. Milestones and acceptance criteria

Effort figures are cumulative and deduplicated. Calendar figures assume the §2.4 blended case
(~5.0 effective pw/week).

### 4.1 v0.1 — `vaco-probe`, byte-identical — **~240 pw, week 50 (~11.5 months)**

**Scope**, exactly as D5 states it and D14.4 amends it:

- Demux **MP4/MOV**, **Matroska/WebM**, **MPEG-TS** (including `mpegtsraw` and m2ts).
- **Parse** H.264/HEVC/AV1/AAC/Opus stream headers. Parse only — no decode. Plan 15 §5.3 is the
  authority that this is legally clean: parsing an SPS implements no codec, and the pools charge on
  codec units rather than on bitstreams.
- Emit byte-identical output for `-show_format`, `-show_streams`, `-show_packets`, `-show_programs`,
  `-show_chapters` and the version/pixel-format sections, across **all six writers** (default,
  compact, csv, flat, ini, json, xml).
- Protocols `file`, `pipe`, `fd`, `data`, `md5`. **No network.**

**Explicitly out**, and stated because these look cheap and would silently expand the matrix:
`-show_frames`, `-count_frames`, `-analyze_frames` (**D14.4** — they need a decoder, which D5
forbids); every muxer except the utility hash sinks; every other container including AVI/ASF/FLV/Ogg/WAV;
`use_wallclock_as_timestamps`; `-listen`; device formats.

**The acceptance test.** Not "ffprobe works" — this, concretely:

1. **Zero unexplained byte differences** across plan 14 §5.6's full matrix (~9,000 invocations) over
   the MP4/MOV, Matroska/WebM and MPEG-TS corpora, with `conformance/known-gaps.toml` containing
   nothing outside the reviewed divergence allowlist.
2. **`probe_score` exact for every corpus file**, including the deliberately ambiguous tie-break
   corpus. This is the proof that plan 18 §1.5's scoring model is right.
3. **Determinism gate:** every corpus file probed 100 times, on 1 and 16 threads, byte-identical.
4. **Fuzz gate:** 24 hours per demuxer with no panic, no OOM (RSS capped at 512 MiB), no hang (10 s
   per input), no debug-mode arithmetic overflow.
5. **Truncation ladder:** every corpus file truncated at 1%, 5%, …, 95%, probed by both binaries,
   exit codes and output compared.
6. **Memory ceiling:** probing a 4-hour 30 fps MP4 uses under 64 MiB RSS.
7. **All 27 VERIFY experiments** (P1–P7, T1–T5, S1, M1–M7, K1–K4, A1, N1, L1) answered and recorded,
   or explicitly deferred with an owner. Plus plan 14's V1–V12.

**Effort and calendar.** The v0.1 package set below sums to **~240 pw**. Its dependency-critical
path is 44 weeks (§3.3), but at the 4–6 agents waves 1–3 sustain, throughput binds first at
**~50 weeks**. This is the one milestone where the two constraints are close; everywhere after it,
throughput dominates by a factor of five or more.

**Packages:** P0-01…05 · FD-01…03, FD-05…12 · PF-0.0, 0.1, 0.2, 0.4 · QA-01…06, QA-08 ·
F-01, F-02, F-04 · X-01, X-03, X-05, X-06 · D-17, D-18, D-20, D-21 · P-01…P-08 ·
IO-01…03 · FW-01…07, FW-11, FW-12 · SH-01…05, SH-09 · PR-01 · FM-01…11, FM-20 · XF-01, XF-02 ·
CL-01…CL-11.

### 4.2 v0.2 — first decoders — **~420 pw cumulative, week 86 (~20 months)**

**Scope.** `vaco-tx` and `vaco-resample` complete. `vaco-checkasm` and the quality-comparison modes.
Decoders: PCM, ADPCM, FLAC, ALAC, PNG/APNG, GIF, TIFF, EXR, JPEG XL, still-JPEG, the simple image
set, Vorbis, Opus, VP8, FFV1, rawvideo, text subtitles. `-show_frames`, `-count_frames` and
`-analyze_frames` land here per D14.4. Fidelity grading and `docs/codec-status.md` exist **before**
the first wrapped codec ships, not after.

**The acceptance test.**

1. **Per-frame checksums identical** (framecrc / framemd5) to the pinned reference across the codec
   conformance suites: flac-test-files, PngSuite, the VP8 test vectors, the RFC 6716 Opus vectors,
   and the FFV1 self-consistency set.
2. **`-show_frames` byte-identical** across all six writers over the v0.1 corpus.
3. **Every wrapped codec carries a D11 fidelity grade** in `docs/codec-status.md`, established by the
   harness. **Nothing graded Unmeasured or Divergent is in the default build** — this is the
   milestone that makes D11's grading real rather than aspirational.
4. **`vaco-checkasm` covers 100% of registered kernel variants**, zero variants slower than their
   scalar reference, and the tier matrix is bit-identical across `fallback`/SSE2/SSE4.2/AVX2/AVX-512/NEON.
5. **`docs/tx/fixed-point.md` is published and frozen** as a specification codecs depend on.

### 4.3 v0.3 — remux and transcode — **~730 pw cumulative, week 148 (~34 months)**

This is the milestone that matters commercially: **43% of the effort for a tool people can actually
use.** Plan 16 §8.7 names the same point ("that is when `vaco` becomes a transcoder rather than a
remuxer") and it is worth organising the first three years around it.

**Scope.** `vaco-sched`. Muxers: MP4, Matroska/WebM, MPEG-TS, raw, wav/aiff/caf, AVI, FLV, Ogg,
image2, the utility sinks. `vaco-scale` complete. `vaco-filter-core` + `vaco-filter-graph` +
`vaco-filter-framesync` + the 48 T1 filters. The `vaco` binary with `-map`, streamcopy and transcode
paths, `-vf`/`-af`, `-force_key_frames`, `-fps_mode`, `-progress`. Encoders: FLAC, ALAC, PNG, FFV1,
VP8, and AV1 via `rav1e`. Bitstream filters `*_mp4toannexb` and `aac_adtstoasc`. `vaco-codec-exec`.

**The acceptance test.**

1. **Remuxed container bytes identical** to the reference for every T1 muxer over the corpus
   (correctness C2), with `-fflags +bitexact` where the container carries random UIDs.
2. **The ~600-case timestamp differential matrix green** (plan 14 §6.4).
3. `vaco -i in.mkv -c copy out.mp4` and its inverse byte-identical to the reference.
4. **A full transcode is deterministic**: byte-identical output across all thread counts, always
   (plan 12 S10). This is a hard invariant, not a target.
5. `vaco -i x -vf "scale=1280:720,overlay=…" -c:v libaom-equivalent …` produces output within the
   published quality bound, with the bound recorded in `quality.lock` and ratcheted.

### 4.4 v0.5 — the tentpoles, hardware, playback and the network — **~1,190 pw cumulative, week 236 (~4.5 years)**

**Scope.** AV1 decode, Argon-conformant. VP9 decode and encode. Opus encode. Vorbis encode. JPEG
native (the first scheduled D11 native replacement). WebP native lossless routed through our VP8.
`vaco-hw-core` + VideoToolbox + **Vulkan Video** — D13's strategic path, because it is how H.264 and
HEVC reach users from a binary containing no software codec for either. All T2 filters. The text and
ASS stack. The GPU filter path (16 WGSL kernels replacing ~87 upstream per-vendor variants).
Protocols through TLS/HTTPS, RTP/RTSP, HLS, DASH. `vaco-play`. `-filter_complex`, loopback decoders,
graph dumps.

**The acceptance test.**

1. **The Argon AV1 conformance suite passes**, with any failures enumerated and owned.
2. **Hardware decode is differentially identical to our own software decoder** for H.264 and HEVC
   (D13's verification item 1 — the strongest oracle available, and it costs nothing extra because
   T3-01/T3-02 must exist anyway). This is why keeping unshippable decoders in-tree is correct.
3. **HLS and DASH playback of public reference streams**, with `container-structure` comparison for
   the live cases that cannot be byte-exact by construction.
4. **`vaco-play` plays every T1 codec** with A/V sync inside the reference's tolerance, seeks
   correctly with serials, and reports the same stats fields.
5. **The published performance table exists**, per plan 12 §10: every band in §1.3 either confirmed by
   measurement or revised by a PR carrying the measurement, including the places we are slower.

### 4.5 v1.0 — **~1,680 pw cumulative, week 336 (~6.5 years)**

#### What v1.0 cannot mean

Two plans reached the same conclusion independently, from different inventories, and it is now a
well-evidenced project-defining constraint rather than a suspicion:

- **Plan 15 §3.5:** ~300 of ~605 decoders and ~60 of ~186 encoders have no public specification. They
  are the formats where FFmpeg's source *is* the specification.
- **Plan 18 §4.2 / D14:** 192 of 368 demuxers (52%) are in the same position.

D7 forbids implementing from FFmpeg's source. **Therefore roughly half of FFmpeg's inventory is
permanently out of reach**, and one further gap is permanent for a different reason: **`vaco` will
never encode H.264, HEVC or VVC in software** (plan 15 §5.3 recommendation 5). That is a legal
constraint, not a technical one, and no amount of engineering removes it.

**v1.0 therefore cannot mean feature parity, and any roadmap claiming otherwise is lying.**

#### What v1.0 does mean

Six criteria, all testable:

1. **Complete on the specified inventory.** Every codec, container, protocol and filter for which
   (a) a public specification exists, (b) D3 permits the licence, and (c) D4 permits the patent
   posture, is implemented, conformance-gated, and graded **Exact** or **Equivalent** with the
   tolerance recorded and reviewed. Concretely: the T1 and T2 codec sets, the T1 and T2 container
   sets, T1+T2 filters, and the implementable protocol set.
2. **Byte-identical wherever byte-identity is meaningful.** ffprobe output, remuxed container bytes,
   and lossless decode are byte-identical to the pinned reference. Lossy encode is inside a published,
   ratcheted quality bound. Plan 18's finding that **muxing is deterministic** is why containers are
   where D6's requirement is most fully achievable.
3. **No `unsafe` outside `vaco-hw-*` and `vaco-fuzz-alloc`**, asserted by CI across the workspace, with
   every remaining block carrying a `SAFETY:` invariant and `clippy::undocumented_unsafe_blocks` denied.
4. **The legal posture holds and is proved, not asserted.** The published default binary contains no
   patent-encumbered encoder or decoder — CI asserts on the compiled feature list, not on intent. No
   "full" convenience binary exists (D9). Every `*-sys` crate has passed the manual-review audit. Every
   implementation commit carries a provenance trailer naming a specification and section.
5. **Absence is documented.** `docs/why-some-codecs-are-not-included.md` and
   `docs/why-some-formats-are-not-included.md` enumerate every omission with its reason —
   spec-less, patent-encumbered, GPL-derived, or superseded. **These are v1.0 deliverables with
   named owners (XF-05 and its codec twin), not afterthoughts.** Plan 15 §9.4 is right that this is
   the document FFmpeg never wrote and the clearest signal the project means what it says.
6. **Performance is disclosed, not claimed.** Plan 12 §10's S1–S10 criteria met, or each miss
   published with a number and a reason. The headline is that a runtime-dispatching Vaco binary sits
   at ~0.96× a distribution ffmpeg on 1080p H.264 decode before PGO, ~1.00–1.04× after — and a
   published table of every place we are slower, by how much, and why.

**The acceptance test.** The full conformance matrix green with every divergence-allowlist category
under its ratchet cap; `release-check` green (reproducible build verified, signed, notarized, SBOM
emitted, feature assertion passed); both omission documents complete and reviewed by the correctness
owner; `docs/codec-status.md` and `docs/format-status.md` carrying no Unmeasured or Divergent entry
in the default build.

**Explicit non-goals at v1.0**, so nobody is surprised: software H.264/HEVC/VVC encode (never); VVC
decode (plan 15: do not build it); the ~300 spec-less T5 codecs and the T5 container equivalents; the
T4 long tail; device capture formats; DVD and Blu-ray disc structure; a C ABI (D1).

---

## 5. The wave schedule

### 5.1 How to read this

Plan 19's waves are **dependency gates, not calendar phases.** Read as disjoint blocks they imply a
7-year serial march; in reality wave 3 opens while wave 2 is still draining, and wave 4 opens while
wave 3 finishes. Each wave below therefore has an **opens** week (the first package becomes ready)
and a **drains** week (the last package completes), and they overlap.

| Wave | Content | pw | Opens | Drains | Ready | **At once** |
|---|---|---:|---:|---:|---:|---:|
| **0 — Contract** | The workspace, the crate graph, the interface freeze | 5.5 | 0 | 3 | 5 | **1–2** |
| **1 — Foundations** | Layer 0/1 crates, the SIMD substrate, the harness scaffolding, `vaco-codec-core` identity | 80 | 3 | 22 | 21 | **4–6** |
| **2 — Substrate** | I/O, protocol core, format framework, shared helpers, codec DSP and CBS, `vaco-tx`/`-scale`/`-resample`, filter core and graph, `vaco-cli-core`/`-textformat` | 385 | 12 | 100 | 65+ | **4–6** |
| **3 — v0.1** | The three demuxers, `vaco-probe`, the conformance corpus, `vaco-hw-core` bring-up | 90 | 30 | 50 | 16 | **4–6** |
| **4 — Wide** | Every remaining codec, format, filter, muxer, protocol, bitstream filter, hardware backend | 970 | 50 | 300 | **330+** *(was 200+; §11 raised it)* | **6–8** |
| **5 — Integration** | `vaco-sched`, `vaco`, `vaco-play`, PGO/BOLT, release engineering, the omission documents | 150 | 120 | 336 | 22 | **3–4** |

### 5.2 Wave 0 — Contract

**Entry:** nothing. **Agents: 1, with a second joining for P0-04.**

**Packages:** P0-01, P0-02, P0-03, P0-04, P0-05, plus PF-0.0 (which needs no workspace and can run in
parallel with all of it).

**Exit gate — all four, no partial credit:**
1. `cargo check --workspace --locked` green with every body `todo!()`.
2. `cargo doc --workspace` builds.
3. CI runs and is green.
4. **The interfaces are frozen.** After this point a signature change is a coordinated orchestrator
   event at a wave boundary, never an ad-hoc edit (plan 19 §6).

**Why 1 agent.** Plan 19 §6 says this phase is "the only part that is" serial and estimates 2–3 weeks.
It is the highest-leverage three weeks in the project: it converts ~120 crates from a dependency chain
into independent tasks.

### 5.3 Wave 1 — Foundations

**Entry:** wave 0's four gates. **Ready: 22. At once: 4–6** (plan 19 says 14 ready / 4–6 at once; the
count here is higher because plan 13's crates and plan 12's Phase 0 are added).

**Queue order — highest value first, because only 4–6 run at a time:**

| # | Package | Why here |
|---:|---|---|
| 1 | **FD-01** `vaco-core` | Everything depends on it. Nothing else can be integration-tested. |
| 2 | **PF-0.1** `vaco-simd` | Gates every DSP crate in wave 2, and PF-0.0 has already de-risked it. |
| 3 | **F-01** `vaco-codec-core` identity | Per D14.1 this gates `vaco-format-core` and therefore all 40 format crates. Longest downstream reach in the register. |
| 4 | **FD-05** `vaco-opts` (+derive) | Plan 11's largest single unknown; longest lead time. Build the `-h full` differential harness first, the macro second. |
| 5 | **FD-07** `vaco-pixfmt` | 268 formats, generated. The differential extractor is the acceptance criterion and is written first. |
| 6 | **QA-01** `vaco-limits` | **Must land before the first demuxer.** Retrofitting a required constructor parameter across 90 crates is a change nobody ever does. |
| 7 | FD-03 `vaco-bitstream` | Gates every parser and codec. |
| 8 | X-01 `vaco-conformance` comparator core | D6 is a merge gate; the harness has to exist before there is anything to gate. |
| 9 | PF-0.2 `vaco-checkasm` verify mode | Plan 17 D.2: "do not start DSP work before the differential kernel harness exists — retrofitting it is how SIMD projects rot." |
| 10 | FD-02 `vaco-pool` · 11 FD-08 `vaco-sampfmt` + `vaco-chlayout` | Small, unblock `vaco-frame` and `vaco-resample`. |
| 13 | FD-11 `vaco-frame` | Needs FD-07, FD-08, FD-02. The first real serialisation inside the wave. |
| 14 | FD-12 `vaco-packet` · 15 FD-10 `vaco-color` · 16 FD-06 `vaco-expr` | |
| 17 | F-04 registry codegen · 18 X-06 D11 CI checks · 19 X-03 fuzz scaffolding | |
| 20 | QA-02 · 21 QA-05 · 22 QA-06 · 23 QA-08 · 24 QA-04 · 25 QA-03 | Harness, fuzz, xtask, provenance. Provenance is cheap now and impossible to retrofit. |
| 26 | PF-0.3 · 27 PF-0.4 · 28 PF-0.5 · 29 PF-0.6 | Bench and vectorization-check tooling. Can slip into wave 2. |

**Exit gate:** foundations tested; **the differential harness runs against the reference `ffprobe`
binary** and produces a diff (plan 19's wave-1 gate); `vaco-limits` complete with the clippy
allocation bans active; provenance trailers enforced by the commit hook.

### 5.4 Wave 2 — Substrate

**Entry:** FD-01, F-01, PF-0.1, X-01 landed. Does **not** wait for all of wave 1. **Ready: 60+. At once: 4–6.**

This is the longest wave — 385 pw over ~88 weeks — and it is where queue discipline earns the most,
because a badly ordered queue here delays v0.1 directly.

**Queue order, in four bands:**

**Band A — the v0.1 spine (dispatch first, always).** F-02 → FW-01 → FW-02 → FW-03 → FW-04 →
{FW-05, FW-07, FW-06} → IO-01 → IO-02 → IO-03 → {SH-01, SH-02, SH-03, SH-04, SH-05, SH-09} →
FW-11 → FW-12 → PR-01. **Nothing else is dispatched while a band-A package is unassigned and ready.**

**Band B — v0.1's codec side, parallel with A.** D-17 → D-18 → {D-20, D-21} → {P-01…P-08} → P-05.

**Band C — v0.1's CLI side, parallel with A and B.** CL-01 → {CL-02, CL-03} → CL-05 → {CL-06, CL-07} → CL-04.

**Band D — post-v0.1 substrate, backfill whenever A/B/C have no ready package.** Ordered by downstream
reach:
1. **SP-C1→C2→C3→C6** — `vaco-tx`'s derived transforms gate *every* transform-coded audio codec.
   Plan 17 D.1.2: "`vaco-tx` first among equals… If staffing forces a choice, `vaco-tx` starts first."
2. **SP-A1→A2→A3→A8** — `vaco-scale`'s spine. Longest critical path of the three SP crates, so
   starting it late makes it the schedule. Start it early even if it progresses slowly.
3. **SP-B1→B7→B8** — `vaco-resample`. Plan 17: **the best first assignment for an agent new to the
   project** — self-contained, classical mathematics, unambiguous correctness feedback, and it teaches
   the `vaco-opts` idiom every later crate uses.
4. **FT-1.1→1.2→1.3→1.5→1.7** — `vaco-filter-core`. 21.5 pw gating ~190. Plan 16: staff it with the
   most senior available agents and do **not** parallelise it beyond three lanes; the adapters are
   written against by ~560 filters.
5. **D-05, D-06, D-11, D-01, D-02** — the cheap shared DSP every audio codec needs.
6. **PF-3.2 batched dispatch** — must settle **before F-02's DSP traits freeze**. If it slips past the
   freeze it becomes a coordinated interface change across every codec crate.
7. **D-08 (mc), D-09 (intrapred), D-10 (deblock)** — the expensive shared DSP. D-10 starts with a
   measurement spike, not an implementation.
8. F-03 `ProgressPicture` — **land it with the synthetic band-straddle benchmark before any codec
   depends on it.** Plan 15 calls it the highest-risk design item in the document.
9. **PR-05's TLS provider benchmark, pulled forward from wave 4.** It is one week and it gates 53.5 pw
   of streaming work. Running it here rather than in wave 4 converts a schedule risk into a fact.
10. The remaining SP, FT-1.x, FT-2.x, D-*, CL-* and QA-07 packages.

**Exit gate:** v0.1's dependencies met (band A, B, C complete); `vaco-tx`, `vaco-scale` and
`vaco-resample` each carrying a fidelity grade and a benchmark baseline; `vaco-filter-core`'s adapter
API reviewed and frozen; the TLS provider decision recorded.

### 5.5 Wave 3 — v0.1

**Entry:** FW-04 and the six v0.1 shared helpers landed. **Ready: 16. At once: 4–6.**

**Queue order:** FM-01 → FM-06 → FM-09 (three demuxer cores in parallel, one agent each) →
FM-02, FM-07, FM-10 → FM-03, FM-08, FM-11 → FM-04, FM-05 → FM-20 → CL-08 → CL-09 → XF-01, XF-02 →
CL-10 → CL-11. In parallel and off the critical path: H-01 and H-02 bring-up (D13 sequences hardware
early, not late).

**Reserve four of the twenty weeks for conformance triage.** Plan 18 §7.2 budgets weeks 12–14 for
it and is explicit that this is "the difference between 'v0.1 slips by a month' and 'v0.1 ships'".
Every field that differs is a real behavioural question requiring an experiment, and there are ~9,000
invocations.

**Exit gate: the seven acceptance criteria in §4.1. D5 met.**

### 5.6 Wave 4 — Wide

**Entry:** the v0.1 gate. **Ready: 330+. At once: 6–8.** 970 pw over ~254 weeks. The ready count rose by ~130 in the §11 pass without the pw moving at all — that is the whole point of the exercise: the queue is deeper, so the 6–8 agents are less likely to be blocked waiting on a package that is *ready but too big to hand to two people*.

This is 58% of the project and it is where plan 19's crate-per-agent model pays off. Plan 19 is right
that it is "almost perfectly parallel *logically*" and equally right that logical parallelism is not
machine parallelism. **The queue is deep and the throughput window is narrow; the ordering below is
what keeps the highest-value work flowing first.**

**Queue order, by tier:**

**Tier 1 — makes `vaco` a transcoder (dispatch until exhausted).** FW-08 → FW-09 → FM-21 → FM-24 →
FM-25 → FM-26 → FM-20; C-01, C-03, C-05, C-08, C-13, C-47; C-46 (`vaco-codec-exec` — 4 pw that
answers the project's most-asked question); FT-4.1, FT-4.2, FT-4.3; B-01, B-02, B-04; C-27; C-06,
C-07; FM-27, FM-28, FM-29, FM-30, FM-35.

**Tier 2 — the tentpoles.** The AV1 chain C-34…C-44 (five agents after one clears C-34…C-37);
the VP9 chain C-29…C-32; C-16→C-19 (VP8, which WebP lossy then routes through); C-20, C-22…C-25
(Vorbis and Opus decode); C-15 (**native JPEG — plan 15 §4A.4's first scheduled D11 replacement**,
because `zune-jpeg` has no MJPEG framing, no 12-bit and no spec-exact IDCT mode); H-06 Vulkan Video.

**Tier 3 — competitive coverage.** **T3-01a–c and T3-02a–c (the H.264 and HEVC bitstream spines) —
promoted here from tier 4 by §11.2.** They are cheap (10 pw each) and they sit at the head of the two
longest chains in the register; starting them in tier 4 is what made those chains long. Then:
FT-4.4…FT-4.11; FT-3.1…FT-3.5; T2-01…T2-06; FM-31, FM-32, FM-33,
FM-34, FM-36, FM-37, FM-38; PR-02…PR-08; FM-40…FM-46 (RTP/RTSP, HLS, DASH); C-45 (rav1e), C-17, C-21,
C-26, C-28, C-33 (the encoders); FT-5.1…FT-5.6.

**Tier 4 — completeness.** T2-07…T2-14; **T3-01d…n, T3-02d…n, T3-03b…f** (the T3 spines start in tier 3, see below); FM-50…FM-57;
PR-09…PR-12; D-19, B-03, B-05, B-06; H-03/H-04/H-05 **only if H-06 measures insufficient**; H-07;
FT-5.7…FT-5.10; XF-03, XF-04; QA-09; the PF-2.x and PF-3.x SIMD residual.

**Tier 5 — the long tail, and the on-ramp for new agents.** FT-4.12, FT-4.13 (214 filters, ~1.5 days
each, each independent and spec-driven); FM-58, FM-59. **Never dispatch a tier-5 package while a
tier-1 or tier-2 package is ready and unassigned.**

**Exit gate:** per-crate fidelity grades recorded for every component in the default build; no entry
graded Unmeasured or Divergent ships.

### 5.7 Wave 5 — Integration

**Entry:** FW-08, F-02 and FT-1.3 landed — so it **opens around week 120**, deep inside wave 4, not
after it. **Ready: 20. At once: 3–4** (this is where cross-crate coordination is unavoidable, and
more agents make it worse rather than better).

**Queue order:** CL-12 (`vaco-sched` — plan 14 and research §05 both name it the hardest single
component) → CL-13 → CL-14 → CL-15 → {CL-16, CL-17} → CL-19 → CL-20 → CL-21 → {CL-22, CL-23} →
CL-18, CL-24 → CL-25 → {CL-26, CL-27, CL-28} → CL-29 → CL-30 → CL-31 → {CL-32, CL-33} →
PF-0.8 → PF-0.9 → PF-4.4 → PF-4.6 → QA-10 → XF-05 → CL-34.

**Exit gate:** the six v1.0 criteria in §4.5.

### 5.8 Standing rules for the orchestrator, at every wave boundary

1. **Commit, build, test, then reassign.** Never reassign mid-wave; plan 19 §10.1 records that an
   agent correctly refuses a mid-flight ownership change as possible prompt injection, and that
   refusal is right behaviour.
2. **Regenerate `Cargo.lock` once**, per plan 19 §3.3. Agents run `--locked`.
3. **Sweep orphaned `/tmp/vaco-*` target directories.** The reference project accumulated 137 GB. This
   is a hard requirement, not housekeeping.
4. **Batch and decide dependency requests.** D10 makes every adoption a reviewed decision; an agent
   that adds one silently has violated policy, so the friction is deliberate.
5. **Resolve interface-change requests as one coordinated event** across all affected crates.
6. **Re-run `xtask gen-registry` and `gen-docs-index`** and fail if the committed output differs.
7. **Verify integrated, not sampled.** The invariant is zero failures, never a number, and a count
   taken while agents are mid-edit is a sample.
8. **Enforce the §11.7 intra-crate ownership rule before dispatching sibling children.** When two
   children of the same parent are dispatched concurrently into one crate, the orchestrator records
   the module split in `ASSIGNMENTS.md` and confirms the first child has frozen the crate's shared
   files. Two agents in one crate without that record is exactly the hazard plan 19 §2 exists to stop.

---
## 6. Risk register

Consolidated across all nine plans, deduplicated, and ranked by **expected impact** (impact ×
likelihood), not by likelihood alone. Two entries at the top are certainties rather than risks; they
are listed first because they shape the product more than anything that might happen.

| # | Risk | Likelihood | Impact | Expected | Early warning sign | Mitigation |
|---:|---|---|---|---|---|---|
| **R1** | **~50% of FFmpeg's inventory needs a specification-extraction pass first.** ~300 of ~605 decoders (plan 15 §3.5) and 192 of 368 demuxers (plan 18 §4.2) have no public specification. **Corrected by D15:** these are *not* legally out of reach — 17 U.S.C. §102(b) excludes procedures and methods from copyright "regardless of the form in which it is described", and format-dictated tables are the paradigm merger case. The constraint is **cost and demand**: 0.5–3 pw of spec extraction per format (250–750 pw across the tail), sample media for verification, against near-zero usage for most entries. Genuinely blocked: trained model weights (`nnedi`) and non-functional authorial tables only — a handful, not hundreds. | **Certain** | **Bounds scope, does not define it** | **High** | External: issues framed as "vaco can't open X". | Publish `docs/format-support.md` listing what is supported, what is deprioritised, and how to request a format — framed as a **roadmap the user controls**, not a permanent hole. Add a **costed spec-extraction track** so any format can be pulled forward when demand justifies it (0.5–3 pw extract + implement). Ship `vaco-codec-exec` (C-46, 4 pw) as a convenience, no longer as the answer of last resort — it drops in priority per D15. Position as "complete on the supported inventory, extensible on request". |
| **R2** | **Throughput ceiling.** Plan 19 §7 bounds concurrency at 4–8 agents on a 10-core/16 GB machine, with memory binding before CPU. At ~5 effective pw/week the calendar to v1.0 is ~6.5 years while the critical path is 53.5 weeks (§3.3). | **Certain** | **High** | **Highest** | Wave-4 queue depth rising while completed-pw/week stays flat. Six agents at `-j 4` already oversubscribe ten cores. | This is a purchasing decision (§8 item 2), not an engineering one — more RAM and cores buy nearly linear speedup to ~8–10 agents. Failing that, **scope discipline**: v0.3 is a usable transcoder at 43% of the effort, and §1.16's reductions plus the post-1.0 backlog are 941 pw of optional work. |
| **R3** | **No software H.264/HEVC encoder, ever** (plan 15 §5.3 rec. 5). A legal constraint, not a technical one. For a tool positioned against ffmpeg this is the most visible functional gap in the project. | **Certain** | **High** | **Very high** | None needed. | Hardware encode via H-02 (VideoToolbox) and H-06 (Vulkan Video) — **D13 sequences these early precisely for this reason**. `vaco-codec-exec` for x264/x265 via the process boundary, which solves the GPL problem and the patent problem at once (D9). And the omission document, written honestly. |
| **R4** | **Clean-room contamination.** A contributor who has read FFmpeg writes the module they read. Existential: it would invalidate the project's central claim. | Medium | **Very high** | **Very high** | A provenance trailer that cannot name a specification section. A `similarity-scan` hit. A PR whose structure mirrors upstream's. | The module-scoped contamination rule (D9: tiered, not blanket) + provenance trailers enforced by hook and CI (QA-08) + `similarity-scan` on an isolated runner. **A written clean-room opinion from counsel** (§8 item 1). An ex-FFmpeg reader can work on everything *except* the module they read. |
| **R5** | **AV1's legal position deteriorates** (D9). Dolby — not an AOMedia member, so under no royalty-free obligation — sued Snap on 2026-03-23 (D. Del. 1:26-cv-00317) over AV1 and HEVC, **seeking an injunction**. AV1 is the flagship of the default build and our largest shipped codec at 70 pw. | Medium | **Very high** | **Very high** | An injunction granted, a second non-AOM plaintiff filing, or an AOMedia member settling on terms that concede validity. | Track the docket — D9 already requires it. The feature-flag machinery means AV1 can move behind an opt-in without an architectural change. VP9 (48 pw, decode+encode) remains a shippable royalty-free fallback and is already in the plan. **Do not let the default build's story rest on AV1 alone.** |
| **R6** | **The widening-multiply-add SIMD gap** (D12 addendum). `fearless_simd` has no composition for the `pmaddwd`/`pmaddubsw` shape: **~6× for `pmaddwd`, ~2.2–2.5× on the 8-tap u8 FIR** — materially worse than plan 12's original ~1.4× estimate. It is exactly the shape H.264/HEVC motion compensation and SAD/SATD are built from. Motion compensation moves to 0.75–0.95×; SAD/SATD to 0.70–0.90×. | **Confirmed present**; the question is magnitude | High | **High** | **PF-0.0 item 1**: build the 8-tap u8 horizontal FIR both ways and count instructions with `llvm-mca`. Pass ≤2.5×; **>3× ⇒ stop and escalate**. | Measure in wave 0, before `vaco-simd`'s API freezes and before one production kernel is written — this is the point of maximum leverage and it costs half a person-week. **Raise the gap upstream before `fearless_simd` 1.0 ships**; the project is actively taking feedback. PGO recovers most of it (0.96× → 1.00–1.04×). And hardware delegation makes H.264/HEVC MC far less load-bearing than it looks. |
| **R7** | **`fearless_simd` concentration.** The entire DSP layer rests on a 417-star, 311-commit crate from one small project (Linebender). It is forkable — that is why it clears Gate 3 — but forking is a cost we would actually pay, not a theoretical escape hatch. | Medium | High | **High** | Upstream release cadence stalling; an unfixed operation gap blocking a kernel; the v1.0 release slipping past early September 2026. | D11's adapter keeps the **interface** blast radius to `vaco-simd` alone, but a substrate change would still mean rewriting kernel bodies across the codebase. **Engage upstream now, before v1.0.** Re-run the Gate 3 assessment and `cargo-geiger` against 1.0 and record both (PF-0.0 item 8). Note also that `kernel!` expands `unsafe` into the calling crate and is therefore closed to us — **`dispatch!` only**, which bounds what we can reach. |
| **R8** | **`vaco-format-core`'s surface is wrong** and churns all 40 format crates. FW-01/FW-02 are frozen in wave 2 and every container waits on them. Unlike `vaco-codec-core`, this one has a hard external deadline because D5's v0.1 ships on top of it. | Medium | High | **High** | The first two demuxers both requesting signature changes at a wave boundary. | Review FW-01 and FW-02 hard, then freeze. Plan 19's interface-freeze protocol handles genuine changes as coordinated orchestrator events. Plan 18 is right that "getting `vaco-format-core`'s surface right is worth more than any other five weeks in this subsystem". |
| **R9** | **The banded picture model (F-03) is too slow** for motion compensation. Plan 15 calls it "the highest-risk design item in the document". If it fails, frame threading becomes unusable and AV1/VP8 decode falls behind. | Medium | High | **High** | F-03's own synthetic band-straddle benchmark, run **before** any codec depends on it. | Measure first, implement second. Escape hatches in plan 15 §1.8.3, in order. **If all three fail, escalate as a D2 decision — do not silently reach for `unsafe`.** |
| **R10** | **Conformance triage exceeds its budget.** ~9,000 invocations, and every differing field is a real behavioural question needing an experiment. Plan 18 budgets 3 of 14 weeks and calls it the difference between shipping and slipping. | Medium-high | Medium-high | **High** | The diff count at the first conformance bring-up (wave 3, ~week 41). | Divergence-allowlist governance from day one — "the allowlist is cheapest to discipline when it has three entries, not three hundred". The ratchet caps per category. The 27 VERIFY experiments scheduled as work, not as discovery. |
| **R11** | **The TLS crypto-provider conflict** (D14.2). Both production `rustls` providers (`ring`, `aws-lc-rs`) vendor and compile C and assembly, failing D10 Gate 1; D13's refinement does not rescue them. `rustls-rustcrypto` may be too slow. | Medium | Medium-high | **Medium-high** | PR-05's throughput benchmark against representative HLS/DASH workloads. | **Run the benchmark in wave 2, not wave 4** — it is one week and it gates PR-05/06/07 and FM-43/44/45/46/57, **53.5 pw**. We are a media tool, not a CDN: TLS sits on manifest and segment fetches, not the decode hot path, so a somewhat slower provider is very likely fine. If inadequate, **escalate to a narrow D10 amendment rather than quietly relaxing the gate**. Unplanned benefit of excluding `ring`: it retires D9's per-file licence audit too. |
| **R12** | **E-AC-3's expiry is unverified.** The 2026-01-30 last-patent-expiry rests on "a single hedged secondary source" (D9). Load-bearing and unconfirmed. | Medium | Medium-high | **Medium-high** | Counsel's answer. There is no technical signal. | **Do not ship E-AC-3 until counsel confirms.** It is already gated in plan 15 §3.2. The cost of the mitigation is zero; the cost of being wrong is shipping an encumbered decoder in the default build, which undoes the entire D4 posture. |
| **R13** | **The ops-graph scaler is materially slower** than fused per-format-pair kernels. `vaco-scale` is 48.5 pw and the architecture explicitly starts where upstream is going rather than where it is. | Medium | High | **Medium-high** | Benchmark scenarios 1–3 at the end of SP-A9. | The optimiser (SP-A3) and the fused-pattern chain compiler (SP-A8/A10) exist precisely for this and are **on** the critical path, not deferred. If scenarios 1–3 miss badly, SP-A10's pattern table grows. |
| **R14** | **Shared-tree corruption.** One agent running `git add -A`, `checkout`, `reset` or `pull` destroys every other agent's in-flight work. There is no merge to catch it. | Medium | High | **Medium-high** | A red tree that is not anyone's in-flight edit. Plan 19 §5 warns that a red tree mid-session is *usually* someone's edit — check whether the symbol exists at `HEAD` before blaming a commit. | Plan 19 §2–§5 in full: crate-level single-writer ownership, the orchestrator-only file list, generated wiring files with no human writers, and **private-index commits** (`GIT_INDEX_FILE` + `write-tree` + `commit-tree`) so the shared `.git/index` is never written. Absolute, no exceptions. |
| **R15** | ~~**T3-01/T3-02 unsplit** become the critical path at 79 weeks.~~ ✅ **CLOSED by §11.1.** Both are now fourteen children each, summing exactly to 60.0 and 55.0, and the worst chain fell from 79.5 to 53.5 weeks. | *(closed)* | *(closed)* | *(closed)* | — | Residual: the two crates now carry fourteen concurrent-capable children each, so **§11.7's intra-crate module ownership rule is what replaces this risk.** Two agents in `vaco-codec-h264` without a recorded module split is the new failure mode. |
| **R16** | **A wrapped codec grades Divergent** and leaves the default build, making a native implementation unscheduled work. | Medium-high | Low-medium | **Medium** | The fidelity grade, produced in the same sprint the crate is adopted. | Plan 15 §4A.4 already ranks the likely candidates: JPEG first (no MJPEG framing, no 12-bit, no spec-exact IDCT), TIFF second (CCITT G3/G4, JPEG-in-TIFF gaps). **Measure every wrapped codec in the sprint it is adopted, never later.** C-15 (native JPEG) is already scheduled in wave 4 tier 2 on this basis. |
| **R17** | **The `i32` fixed-point transform contract needs revision** after conformance testing, and conformance vectors may not exist until a codec does. | Medium | Medium | **Medium** | The first fixed-point codec running conformance vectors — potentially long after `vaco-tx` is "complete". | Build the golden-vector machinery early (SP-C10) with *our* chosen contract, so a later change is a table regeneration and a review rather than a rewrite. `docs/tx/fixed-point.md` is versioned with the crate and changing it is a codec-affecting decision. |
| **R18** | **`vaco-opts`' derive** is plan 11's largest single unknown and gates `vaco-cli-core` and every component's option surface. | Medium | Medium | **Medium** | The `-h full` differential harness disagreeing with the reference on schema shape. | Build the harness **before** the macro, so "is the schema right?" is answerable from day one, and write the runtime first so the macro's only job is projection. |
| **R19** | **Rate-conversion coefficients cannot be matched** to the reference (plan 17 §B.14.2) — the largest open question in plan 17. | Medium-high | Low-medium | **Medium** | SP-B14's class-8 investigation, which cannot start until SP-B9 produces coefficients. | Defined fallback: grade **Equivalent** at ≥100 dB SNR plus the independent signal-quality suite. Budgeted at 3 pw and scheduled with slack, so an overrun degrades the fidelity grade rather than blocking the crate. |
| **R20** | **Portable SIMD loses on deblocking** (architecture §7.2 #8 — branchy per-edge decisions, the hardest portable-SIMD target). 10–20% slower decode on the H.264 family. | Medium | Medium | **Medium** | D-10's measurement spike, which comes **before** the implementation. | Masked-lane select is the technique. If it does not land, deblocking becomes a documented performance gap, not a correctness one — and plan 12 §10 already requires publishing every such gap with a number. |
| **R21** | **Disk exhaustion.** Per-agent target directories accumulated to **137 GB** on the reference project. | Medium-high | Low (but it stops the machine) | **Medium** | Free space, checked at wave boundaries. | The cleanup-on-finish rule in every agent brief (`rm -rf` by literal path, **never** `glob /tmp/vaco-*`), plus the orchestrator's sweep at each wave boundary. |
| **R22** | **`rustybuzz` / `ass-core` provenance.** `rustybuzz` is a *port* of HarfBuzz, not a wrapper; if any of it is near-verbatim translation, Old-MIT attribution travels with it. | Low-medium | Medium | **Low-medium** | The provenance review itself. | Resolve before FT-3.5 starts — the whole text stack sits on it (~19 pw: FT-3.5, FT-4.10, FT-5.2, FT-5.3). We plan to write our own ASS renderer regardless, so `ass-core` is lower stakes. |
| **R23** | **Fuzzing 500+ targets is a compute bill** — 250 CPU-hours a night at 30 min per target. | High | Low | **Low-medium** | The first full nightly run. | Rotation, almost certainly: each target runs every N nights, prioritised by recent code change and by new-coverage rate. Specify the scheduler at v0.3 (QA-07). |
| **R24** | **Paid specifications and conformance suites.** ISO 14496-3, ISO 14496-26, ITU-T T.83, ISO 11172-4. | High | Low | **Low-medium** | Blocked conformance gating on MP3 (GREEN and shippable, so its suite is a real budget question) and JPEG. | Budget it — a few thousand euro is trivial against 1,680 person-weeks. AAC is easy to defer since D9 makes it RED anyway. |
| **R25** | **Argon AV1 stream licensing** is assumed rather than recorded, and Argon is by far the best AV1 coverage set. | Low | Medium | **Low-medium** | Reading the actual published terms at adoption. | Record the terms at adoption (X-05/QA-09), not at gating time. If they prohibit our use, C-44's conformance gate needs a substitute and AV1's grade weakens. |
| **R26** | **Estimates are optimistic because option surfaces are under-appreciated.** Plan 17 §D.3 item 8 names this and recommends treating a 25% overrun as the planning case. | Medium-high | Medium | **Medium** | Consistent per-package overruns in waves 1–2, where the packages are small enough to see the pattern early. | Plan 17 already carries explicit line items for option surfaces (SP-A12, SP-B12) and fidelity probes (SP-A14, SP-B14) — the two things most commonly omitted. **Note honestly that the §2 total does not carry a 25% contingency.** If plan 17's caution generalises, v1.0 is ~2,100 pw and ~8 years. |

**On the missing contingency.** The §2 figure is a *deduplicated sum of the plans' own estimates*, not
a risk-adjusted forecast. Plan 17 recommends a 25% overrun allowance and plan 18's MP4 risk row makes
the same point in a different way. Applying it uniformly gives **~2,100 pw and ~8 years**. This
document does not apply it, because inventing a contingency the domain plans did not ask for would
make the number less traceable, not more honest. **Read 1,680 as the planning case and 2,100 as the
pessimistic case, and treat any slip that stays between them as within forecast.**

---

## 7. The first ten work packages

Ordered for immediate dispatch. Packages 1–5 are wave 0 and run at concurrency 1–2; 6–10 open wave 1
and run at concurrency 4–6. Every brief carries plan 19 §8's task contract and §4.2's build-isolation
rule **verbatim**, and plan 19 §10.1 is binding: **scope, file ownership and constraints go in the
initial brief and are never changed afterwards.**

---

**#1 — P0-01 · Workspace configuration** — 1.0 pw · orchestrator · no deps

- **Owns:** `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`, `deny.toml`,
  `about.toml`, `.cargo/config.toml`. These are on plan 19 §3.6's orchestrator-only list forever after.
- **Do:** root manifest with `members = ["crates/*/*", "xtask"]` (glob — so no agent ever edits it to
  register a crate) and `resolver = "3"`. **Pin stable 1.89, not nightly** — D12's addendum retires the
  `portable_simd` requirement; Cranelift dev builds become an opt-in environment variable in one `just`
  recipe. Pre-populate `[workspace.dependencies]` with every external crate plans 11–18 anticipate,
  each with its version. Lint configuration per plan 11 §1. Profiles: `lto = "fat"`,
  `codegen-units = 1`, `panic = "abort"` for binaries. `deny.toml` per D3's allow/deny lists with
  MPL-2.0 denied. **Set `rustc-wrapper = sccache` now** — plan 19 §4.4 records that adding or removing
  it later forces a full rebuild in every target directory.
- **Distribution baseline is x86-64-v2, not v3** (D12 addendum): runtime dispatch makes a lower floor
  strictly better.
- **Done when:** `cargo metadata` succeeds; `cargo deny check` runs; `sccache --show-stats` responds.

---

**#2 — P0-02 · The crate graph** — 1.0 pw · same agent · deps: P0-01

- **Owns:** every directory under `crates/`.
- **Do:** create all ~120 crate directories from `10-architecture.md` §3 with manifests and dependency
  edges, so the graph is real and acyclic from day one. **Apply D14.1**: `vaco-codec-core` sits
  **below** `vaco-format-core`; write `layers.toml` accordingly and add the ban rule that stops any
  format crate depending on any codec crate. Add `vaco-limits`, `vaco-conformance`, `vaco-corpus`,
  `vaco-checkasm`, `vaco-fuzz-support`, `vaco-fuzz-alloc` and `xtask` from plan 13 §0.2 — they are
  additions to architecture §3 and are easy to forget. Every crate gets `#![forbid(unsafe_code)]` in
  its `lib.rs` except the D2 allowlist (`vaco-hw-*`, `vaco-fuzz-alloc`).
- **Done when:** `cargo check --workspace` compiles empty crates; `xtask layer-check` passes on an
  empty graph.

---

**#3 — P0-03 · The interface freeze** — 1.5 pw · same agent, **everyone reviewing** · deps: P0-02

- **Do:** write out every public type, trait and function signature, compiling, bodies `todo!()`:
  `vaco-core`'s error taxonomy and `Rational`; `Frame`/`Packet` and the full `SideData` set;
  `Demuxer`/`Muxer` + `DemuxCtx`/`MuxCtx` + `ParserProvider`/`BsfProvider`;
  `Decoder`/`Encoder`/`Parser`/`BitstreamFilter` and the send/receive model;
  `Filter`/`Activity`/`FilterCtx`; `vaco-opts`' derive shape; `KernelSet`/`Tier`/`Variant`;
  `Limits`.
- **This is the highest-leverage day and a half in the project.** It converts ~120 crates from a
  dependency chain into independent tasks. Review it as if nothing can be changed afterwards, because
  after this a signature change is a coordinated orchestrator event at a wave boundary.
- **Done when:** `cargo check --workspace --locked` green; `cargo doc --workspace` builds; the
  reviewers have signed off; **the freeze is announced.**

---

**#4 — P0-04 · Tooling and CI** — 1.0 pw · second agent · deps: P0-01

- **Owns:** `xtask/`, `Justfile`, `.github/`.
- **Do:** `xtask` with `gen-registry` (walks `crates/**/vaco-component.toml`, emits the registry
  source, committed and CI-verified), `gen-docs-index`, `layer-check`, `dep-gate` (asserts every
  third-party media crate appears in **exactly one** `Cargo.toml` under `crates/` — D11's boundary
  made real). `Justfile` with every cargo recipe expanding to the `--target-dir` **flag** form; note
  that a `cargo xtask` *alias* cannot carry `--target-dir`, which is exactly why `just xtask` exists.
  CI: `cargo check --workspace`, `cargo doc`, `cargo fmt --check`, `cargo clippy`, `cargo deny`,
  `layer-check`, with `mozilla-actions/sccache-action` pinned to the exact local sccache release.
- **Never set `CARGO_TARGET_DIR` as an environment variable** — sccache hashes `CARGO_*` into its keys
  and the env-var form measured 0% cache hits against 78–94% for the flag form.
- **Done when:** CI is green on the empty workspace and `just ci` reproduces it locally.

---

**#5 — PF-0.0 · The `fearless_simd` adoption checklist** — 0.5 pw · third agent, **needs no workspace** · no deps

- **Do:** all eight measurements in plan 12 §11 against `fearless_simd` v0.7.0. **Items 1, 5 and 7 are
  blocking**; 2, 3, 4, 6 and 8 are recorded and reported.
  1. **The `pmaddubsw` composition.** Build the 8-tap u8 horizontal FIR both ways and count
     instructions with `llvm-mca` at each level. Pass ≤2.5×. **>3× ⇒ stop and escalate.**
  5. **Inlining actually happens.** Take one real kernel through `dispatch_kernel!` and assert with
     `cargo-show-asm` that the AVX2 monomorphisation contains 256-bit instructions and **no `call`**.
     A body that fails to inline is compiled at baseline: correct, silently slow, invisible to every
     correctness test.
  7. **Cross-tier bit-exactness** over the `ops` module and two real kernels at every level including
     `fallback`. Any divergence is a blocking upstream bug report.
- **Why now:** this is the only point at which switching substrates is still cheap, and it gates
  PF-0.1's API. Half a person-week that de-risks the entire DSP layer.
- **Also do:** open the upstream issue about the widening-multiply-add gap. `fearless_simd` v1.0 is
  imminent and the project is actively taking feedback — this is the moment.
- **Done when:** `docs/simd-dispatch.md` carries the measurement table and `docs/dependencies.md`
  carries the D10 adoption record with the Gate 3 assessment and `cargo-geiger` count.

---

**#6 — FD-01 · `vaco-core`** — 2.0 pw · deps: P0-03 · **opens wave 1**

- **Owns:** `crates/core/vaco-core/**`, `docs/core.md`.
- **Do:** the `Error`/`Result` taxonomy; `Rational` with exact arithmetic; timestamps, time bases and
  rescaling **with explicit rounding modes** (this is where ffmpeg's `av_rescale_q_rnd` semantics have
  to be reproduced exactly — get it wrong and every timestamp test in the project fails);
  `MediaType`; the logging façade over `tracing`; the shared newtypes.
- **Spec:** plan 11 §4. Provenance trailer names it.
- **Done when:** unit tests, property tests on `Rational` and rescaling, a fuzz target, `docs/core.md`
  with the five required sections, `cargo test -p vaco-core --locked` green in a private target dir.

---

**#7 — PF-0.1 · `vaco-simd`** — 3.0 pw · deps: PF-0.0, P0-03

- **Owns:** `crates/core/vaco-simd/**`, `docs/simd.md`, `docs/simd-dispatch.md`.
- **Do:** `Tier`, `Variant`, `KernelSlot`, `CpuProfile`, `dispatch_kernel!`, and the `ops` module
  including **all nine D12 gap compositions** — saturating add/sub, rounded average, absolute
  difference, integer abs, horizontal reduction, and the widening multiply-add shim — each with an
  exhaustive test and an **instruction-count assertion** so a regression in the composition is a test
  failure rather than a silent slowdown. Plus the bitstream `scan` idiom `vaco-bitstream` needs.
- **D11 boundary, enforced by CI:** `fearless_simd` is named in this crate's `Cargo.toml` and in
  `[workspace.dependencies]` and **nowhere else**. `xtask dep-gate` asserts exactly one occurrence.
- **Use `dispatch!` exclusively. `kernel!` expands `unsafe` into the calling crate and is closed to
  us** (D12 addendum). `#![forbid(unsafe_code)]` must survive.
- **Done when:** the tier matrix is bit-identical at `fallback`/SSE2/SSE4.2/AVX2/AVX-512 and NEON;
  `docs/simd-dispatch.md` carries F5′ in full, the gap table with its compositions, and the
  measurement that would trigger a D2 amendment.

---

**#8 — F-01 · `vaco-codec-core` identity layer** — 3.0 pw · deps: FD-01, FD-12

- **Owns:** `crates/codec/vaco-codec-core/**` (the identity half), `docs/codec/codec-core.md`.
- **Do:** `CodecId` codegen, `CodecParameters`, `Profile`/`Level`, `Caps`, and the descriptor model
  the registry stores. **Per D14.1 this blocks `vaco-format-core` and therefore all 40 format crates** —
  it has the longest downstream reach of any package in the register.
- **Plan 15: "getting `vaco-codec-core` right is worth more than any other six weeks in the project."**
  Review hard, then freeze the surface.
- **Done when:** the descriptor model round-trips through `vaco-opts` introspection; `-h decoder=x`
  can be answered without instantiating anything; docs and fuzz target present.

---

**#9 — FD-05 · `vaco-opts` and its derive** — 5.0 pw · deps: FD-01

- **Owns:** `crates/core/vaco-opts/**`, `crates/core/vaco-opts-derive/**`, `docs/opts.md`.
- **Do, in this order** — the order is the mitigation:
  1. **The `-h full` differential harness first**, so "is the schema right?" is answerable from day one
     against the reference binary.
  2. **The runtime traits second** — typed, introspectable, string-parsable option sets with ranges,
     defaults, units, named constants, runtime-settable flags.
  3. **The derive macro last**, so its only job is projection onto an already-correct runtime.
- **Why it is early despite being large:** plan 11 names it the largest single unknown in the layer and
  it gates `vaco-cli-core` and every component's option surface. At three agents it *is* the
  foundations critical path.
- **Done when:** compile-fail tests for the macro; the `-h full` harness reports zero divergence on the
  reference's option tables for the covered kinds; fuzz target on option-string parsing.

---

**#10 — FD-07 · `vaco-pixfmt`** — 3.0 pw · deps: FD-01

- **Owns:** `crates/core/vaco-pixfmt/**`, `docs/pixfmt.md`.
- **Do:** ~268 pixel-format variants and their descriptor metadata — plane count, component layout,
  bit depth, subsampling, endianness, flags — **generated from a declarative family table, never
  hand-written**, so metadata cannot drift.
- **Write the differential extractor first.** It is the acceptance criterion: it reads the reference
  binary's `-pix_fmts` output plus per-format probes, compares against our generated table, and
  reports divergences. Then iterate the family declarations until it reports zero.
- **Note under D9:** format *names* are interface facts and are free to reproduce; the reference's
  reordered or pre-scaled tables are its expression and are off limits. Ours is derived from the
  spec and from black-box observation.
- **Done when:** zero divergences from the extractor; `vaco-frame` (FD-11) can be built against it;
  fuzz target on format parsing.

---

**Next six in the queue**, so a freed agent is never idle: QA-01 (`vaco-limits`) · FD-03
(`vaco-bitstream`) · X-01 (conformance comparator core) · PF-0.2 (`vaco-checkasm` verify mode) ·
FD-02 (`vaco-pool`) · FD-08 + FD-09 (`vaco-sampfmt`, `vaco-chlayout`).

---
## 8. Open questions requiring the user's decision

Consolidated from all nine plans, deduplicated, and **ranked by how much downstream work each blocks**
— not by how interesting each is. Items 1–3 block more than everything below them combined.

---

**1 — The counsel package.** *(D9's five questions; plan 13 §10.1)*
**Blocks: the legitimacy of all 1,680 pw.** Not a technical dependency — an existential one.

Five things genuinely need a lawyer, and they have lead times measured in months:
(a) contributory-infringement exposure for the AMBER codecs, chiefly AV1 given *Dolby v. Snap*;
(b) **the jurisdiction of the entity that ships binaries — decide before incorporating**;
(c) a commissioned freedom-to-operate search;
(d) **a written clean-room opinion**, which is what makes D7 a defence rather than a policy;
(e) trademark clearance for the `vaco` name and any opt-in `ffmpeg`-named compatibility shim.

Add a sixth from D13: confirm that feeding a bitstream to a licensed hardware decoder is clean. It
almost certainly is — it is what every media application on every platform already does, and we ship
no patented decoding logic — but the **entire default-build strategy now rests on it**, so it is worth
a sentence in the written opinion.

**Recommendation: start this before the first line of code.** It has the longest lead time of anything
in this document and it gates nothing technically, which is exactly why it gets deferred until it is
urgent.

---

**2 — Hardware and concurrency budget.**
**Blocks: the slope of the entire schedule.**

Plan 19's 4–8 agent ceiling was measured on a 10-core / 16 GB machine, with **memory binding before
CPU**. §2.4 shows the calendar is almost linear in sustained agent count between 4 and 8. Doubling
memory and cores plausibly halves the calendar — from ~10 years at 4 agents to ~5.4 at 8.

Separately, plan 12's D-P8: **two dedicated benchmark machines (x86-64 + AArch64) are a hard
prerequisite** for §4's performance gating. Cloud runners record results but must never gate. This is
a purchasing decision with lead time and it is needed before PF-0.5.

**Recommendation: decide the machine budget now.** It is the highest-leverage single decision
available and no amount of engineering substitutes for it.

---

**3 — Is v1.0 allowed to mean "complete on the specified inventory" rather than parity?**
**Blocks: every scoping conversation, and all public communication.**

§4.5 argues it must, because ~50% of FFmpeg's codec and format inventory has no public specification
and D7 forbids the only other route. If the answer is no — if v1.0 must mean parity — then the project
as scoped cannot reach v1.0 and the decision documents need reopening, starting with D7.

**Recommendation: yes, and say so publicly from the first commit rather than at launch.** The two
omission documents are the deliverable that makes this credible, and plan 15 is right that they are
the document FFmpeg never wrote.

---

**4 — The TLS crypto provider.** *(D14.2; plan 18 §9.1.1)*
**Blocks: PR-05, PR-06, PR-07, FM-43, FM-44, FM-45, FM-46, FM-57 — 53.5 pw.**

D10 Gate 1 excludes both production `rustls` providers; D13's refinement does not rescue them.
D14.2 provisionally chose `rustls-rustcrypto` **subject to a throughput benchmark**.

**Recommendation: run the benchmark in wave 2, not wave 4.** It is one week and it converts a
schedule risk into a fact. If `rustls-rustcrypto` proves inadequate, escalate to a **narrow, named**
D10 amendment — never quietly relax the gate, because the gate is what makes the pure-Rust guarantee
checkable rather than aspirational.

---

**5 — Splitting T3-01 and T3-02.** ✅ **RESOLVED by §11.1** — split into fourteen children each along
the specifications' own structure, summing exactly to 60.0 and 55.0. The worst chain fell from 79.5 to
53.5 weeks and no package over 8 pw remains anywhere in the register.

**What is still the user's to decide is the half of this item that was never a planning question:
do T3-01 and T3-02 belong in v1.0 at all?** Plan 15 §9.2 is right that they must never become the
project's centre of gravity just because H.264 is the codec everyone knows, and they are 115 pw of
decoders we are never allowed to ship. Their justification is D13's verification story: they are the
oracle that validates the hardware decoders. **Note that they are now also the critical path** (§3.3
chain ε) — dropping them takes it from 53.5 to 52.5 weeks, which is to say dropping them buys 115 pw
of throughput and one week of calendar. Decide it on the throughput, not on the calendar. See §9.14.

---

**6 — `rustybuzz` and `ass-core` provenance.** *(plan 16 §9.1–9.2)*
**Blocks: FT-3.5, FT-4.10, FT-5.2, FT-5.3 — ~19 pw and the entire text stack.**

`rustybuzz` is a *port* of HarfBuzz, not a wrapper. If any of it is near-verbatim translation, Old-MIT
attribution travels with it. `ass-core` raises the same question against libass at lower stakes, since
we plan to write our own renderer regardless — it only decides whether we can reuse its parser.

**Recommendation: resolve before FT-3.5 starts.** The whole text stack sits on the answer.

---

**7 — VMAF: implement, substitute, or omit?** *(plan 13 §10.5; plan 16 item 5.7)*
**Blocks: FT-5.7 (10 pw) and the completeness of X-04's lossy-encoder grading.**

Netflix's VMAF is BSD-2 but is a C library, so D10 Gate 1 excludes it. Three options: find a pure-Rust
implementation clearing the gates; implement the model ourselves from the published papers (10 pw); or
**ship PSNR + SSIM and say so.**

**Recommendation: plan 13's own "honest default" — the third**, with FT-5.7 kept in the register as
optional. Under D11 a lossy encoder needs *some* quality gate to escape "Unmeasured", and PSNR+SSIM
plus a published bound is sufficient for that; VMAF is a nicety, not a gate.

---

**8 — SRT: build native, or omit?** *(plan 18 §9.1.3)*
**Blocks: PR-10 — 12 pw.**

The SRT bindings are MPL-2.0 and excluded by D3. Plan 18 recommends a native implementation from
`draft-sharabayko-srt`, tiered T3, at 12 pw. Needs a yes/no so the roadmap carries it or not.

**Recommendation: yes, but post-v0.5.** It is real user demand in the broadcast contribution market
and it is one of the few network protocols with a genuine specification.

---

**9 — E-AC-3.** *(D9)*
**Blocks: part of T2-04, all of T3-05 — ~6 pw. But the exposure is disproportionate to the effort.**

The 2026-01-30 last-patent-expiry rests on a single hedged secondary source. **Do not ship until
counsel confirms.** Folded into item 1's package; listed separately because it is the one item there
with a concrete code consequence.

---

**10 — The `-show_frames` scoping question.** ✅ **RESOLVED by D14.4** — moves to v0.2.
Recorded here only because plan 18 §9.1.4 still lists it as open and plan 14 §9's v0.1 acceptance
matrix still includes it. **Plan 14 §9's v0.1 table needs correcting**; this roadmap already reflects
the resolution (CL-08's v0.1 scope excludes frame sections).

---

**11 — The layering amendment.** ✅ **RESOLVED by D14.1** — `vaco-codec-core` below `vaco-format-core`.
Recorded because `10-architecture.md` §3 still shows the old order in its layer tables (with an
amendment note above them) and P0-02 must write `layers.toml` from the corrected order, not the
table.

---

**12 — The OS-interface carve-out.** ✅ **RESOLVED by D14.3** — `std` and thin syscall wrappers
(`socket2`, `rustix`-style) are permitted everywhere; pure-Rust bindings to OS media and graphics APIs
(`ash`, `objc2-*`, `windows`, `wgpu`) are permitted in `vaco-hw-*` and `vaco-filter-gpu` only.

---

**13 — Fuzzing compute budget.** *(plan 13 §10.7)* **Blocks: QA-07's design.**
500 targets × 30 min nightly is 250 CPU-hours a night. Buy it, or rotate by recent-change and
new-coverage rate. **Recommendation: rotate**; specify the scheduler at v0.3.

---

**14 — Paid conformance suites.** *(plan 13 §10.4; plan 15 §9.3)* **Blocks: T2-03 and C-15 gating.**
MP3 is GREEN and shippable so ISO 11172-4 is a real budget question; ITU-T T.83 for JPEG likewise.
AAC's suites are easy to defer since D9 makes AAC RED anyway. **A few thousand euro against 1,680
person-weeks. Buy them.**

---

**15 — Who is the correctness owner?** *(plan 13 §10.8)* **Blocks: governance, not code.**
Plan 13 §1.4 and §1.12 hang two CODEOWNERS gates on this role — the divergence allowlist and the
fidelity grades. **Needed before the first allowlist entry, or the governance is decorative.**

---

**16 — Argon AV1 stream licensing.** *(plan 13 §10.3)* **Blocks: C-44's conformance gate.**
Record the actual published terms at adoption rather than assuming BSD. Load-bearing because Argon is
by far the best AV1 coverage set.

---

**17 — Reference-image distribution.** *(plan 13 §10.1)* **Blocks: the first public CI run.**
Is publishing a built GPL/LGPL reference image to our registry, with source URL, hash and build
scripts alongside, sufficient for GPL §3/§6? **If unsure, build in CI every run and publish nothing.**
Low cost either way.

---

**18 — Binary-size budget: ship AVX-512 by default?** *(plan 12 D-P3′)*
**Blocks: nothing; decide when PF-0.0 item 4 lands.** Provisional trigger: >2.5× the single-level size
means reconsider. Packagers will ask, so have the number.

---

## 9. Where the plans disagree, and how it is adjudicated

Fourteen conflicts. Nine are settled by D1–D14 and are recorded so the stale text is visible; five
required adjudication here and are flagged for the user.

### 9.1 Layering: codec-core vs format-core — **SETTLED (D14.1)**
`10-architecture.md` §3 places `vaco-format-core` at layer 3 and `vaco-codec-core` at layer 4. Plan 18
§1.0 argues the reverse: demuxers need codec parameters and bitstream parsers, and the other order
would make every format crate depend on codec crates and collapse acyclicity. **D14.1 rules for plan
18.** Demuxers reach parsers through an injected `ParserProvider`; no format crate ever depends on a
codec crate. The register reflects it (FW-01 depends on F-01). Architecture §3's layer tables carry
the amendment note but still show the old order in the tables themselves — **P0-02 must write
`layers.toml` from the amendment, not from the tables.**

### 9.2 `-show_frames` in v0.1 — **SETTLED (D14.4)**
Plan 14 §9's v0.1 acceptance matrix includes it; D5 scopes v0.1 as parse-only. `-show_frames` reports
decoded-frame properties and therefore needs a decoder. **D14.4 moves it, `-count_frames` and
`-analyze_frames` to v0.2.** Plan 14 §9's table is stale and should be corrected.

### 9.3 SIMD substrate — **SETTLED (D12)**
Architecture §7.3, plan 11 and plan 12 were all written against `std::simd` with const-generic lane
counts. D12 replaces it with `fearless_simd`: fixed-width vectors, level-generic functions, runtime
dispatch. **Plan 12 §4's worked example is still written against the old API and must be rewritten** —
a documentation debt attached to PF-0.1. The D12 addendum adds a constraint neither plan knows about:
**`dispatch!` is safe, `kernel!` is not**, so raw intrinsics are closed to us.

### 9.4 Toolchain: nightly or stable — **SETTLED (D12 addendum)**
D8 and architecture §2/§8 pin nightly for `portable_simd`. D12's addendum moves to **pinned stable
1.89**, with Cranelift dev builds as a per-invocation environment variable. Plan 11 §1 already
reflects this; architecture §2 and §8 are stale.

### 9.5 Hardware crates in the default build — **SETTLED (D13), plan 15 stale**
Architecture §3 lists `hw-*` under "Optional — the unsafe allowlist". **D13 corrects this
explicitly**: hardware acceleration is in the default build wherever it is legally distributable and
the platform supports it, and containing `unsafe` was never a good reason to exclude it.
**Plan 15 §8's closing paragraph still says the `hw-<backend>` features "stay behind non-default
features and out of the published default build until enabled per platform" — that directly
contradicts D13 and is stale.** D13 governs; the register schedules H-01/H-02/H-06 in wave 3.

### 9.6 Which hardware backends — **ADJUDICATED HERE; flagged**
D13's table names Vulkan Video as "the best single investment", VideoToolbox as required (MoltenVK
does not implement Vulkan Video, so Apple cannot be reached through the Vulkan path), D3D12 as
optional since Vulkan already covers Windows, and **VA-API/NVDEC as "only if Vulkan Video proves
insufficient in practice. Prefer not to."** Plan 15 §7.5 schedules `vaco-hw-vaapi`, `vaco-hw-d3d11`
and `vaco-hw-nvdec`/`-nvenc` as first-class 11 pw items each, and does not schedule Vulkan Video ahead
of them.

**Adjudication: D13 governs.** H-06 (Vulkan Video) is promoted to wave 3 alongside H-01 and H-02;
H-03/H-04/H-05 become **conditional on H-06 measuring insufficient**, worth **−33 pw** if Vulkan
Video delivers. Note also that plan 15 §7.5 names `vaco-hw-d3d11` + Media Foundation while D13 names
**D3D12 Video** — a different API. **Flagged for the user: this is a 33 pw scope decision resting on
an unmeasured claim about Vulkan Video's real-world coverage.**

### 9.7 `vaco-tx` effort — **ADJUDICATED HERE**
Plan 15 D-16 budgets **8 pw**. Plan 17 Part C budgets **27 pw** (29 with the deferrable C13). Plan 12
budgets **8 ew for its SIMD alone**. Plan 17 owns it: it is the only plan that specifies the bit-exact
`i32` fixed-point arithmetic contract that D10 §"the judgement call" names as the reason `vaco-tx` is
ours rather than `rustfft`'s, and `docs/tx/fixed-point.md` is normative for codec conformance rather
than descriptive. **Plan 15 under-costed `vaco-tx` by 3.4×**; D-16 is struck.

### 9.8 `vaco-checkasm` and `vaco-simd` effort — **ADJUDICATED HERE**
Three plans budget each crate independently (checkasm: 5 / 3 / 2 pw; simd: 3 / 3 / 3 pw). Plan 12 owns
both, being the only plan that itemises verify mode versus bench mode, the `perf-event` backend and
the nop-baseline protocol, and the nine D12 gap compositions. **−10.5 pw.**

### 9.9 Kernel ownership generally — **ADJUDICATED HERE, from plan 19**
Plan 12 organises 132.5 pw of Phase 1–3 kernel work into "tracks" that would write into `vaco-scale`,
`vaco-resample`, `vaco-tx`, the filter crates and the codec DSP crates. **Plan 19 §2 forbids this:
"An agent assigned `vaco-codec-flac` owns `crates/codec/vaco-codec-flac/**` and nothing else."** A
plan-12 track cannot be the writer of a kernel living in another agent's crate.

**Adjudication: plan 12 owns the kernel-authoring standard (§2's template), the verification tooling
(`vaco-checkasm`, `vaco-vecheck`), the benchmark suite and the measurement programme. The domain
plans own the kernels.** This is the single largest deduplication in §2.3 (−102.5 pw) and it is
adjudicated on an execution constraint, not on a judgement about the estimates. Plan 12's §8 table
should be re-read as *scope for other people's crates*, not as an independent work queue.

### 9.10 Plan 15's own two totals — **ADJUDICATED HERE**
§4.15's T1 roll-up says **383 pw**; §7's package tables for the same waves sum to **439.5 pw**. §4.15
omits X-01…X-06, D-12…D-15, D-19, D-22 and C-46 — all real work. **§7 governs; §4.15 is a summary of a
subset and the two must never be added together.**

### 9.11 Plan 16's arithmetic — **CORRECTED HERE**
§8.7's cumulative table reports 228.0; the phase subtotals sum to 224.0. Items 3.5 and 4.10 each
appear in two cumulative rows. **224.0 is correct.**

### 9.12 Plan 12's arithmetic — **CORRECTED HERE**
Phase 0's items sum to 19.5; the section header says 19 and the summary table says 20. The plan total
is **168.0**, not 168.5.

### 9.13 Plan 18's Wave F5 arithmetic — **VERIFIED CORRECT**
Recomputed independently: 115.5. All six of plan 18's wave subtotals and its 455.5 total check out.
Plan 18 has the most reliable arithmetic of the nine plans and its work breakdown was used as the
template for this register's format.

### 9.14 The v1.0 codec scope — **FLAGGED, NOT ADJUDICATED**
Plan 15 §9.1 puts T2-07…T2-14 and T3-01…T3-03 inside v1.0 (~700 pw cumulative for codecs alone).
Plan 15 §9.2 simultaneously argues H.264 "must never be allowed to become the project's centre of
gravity" and that VVC should not be built at all. Those are consistent, but the v1.0 line drawn at
"T2 complete, T3 in-tree" is a judgement, not a derivation — and it is **145 pw of decoders we can
never ship, plus 88 pw of T2-07…T2-14**.

**This roadmap carries plan 15's line** because the D13 verification story genuinely needs T3-01 and
T3-02 as an oracle for the hardware paths. **But it is the largest discretionary block in the v1.0
number and the user should confirm it.** Removing T2-07…T2-14 and deferring T3-03 would take v1.0 to
~1,560 pw and ~6.0 years.

---

## 10. What this roadmap deliberately does not do

- **It does not add a contingency.** §2 is a deduplicated sum of the plans' own estimates. Plan 17
  recommends 25%; applied uniformly that is ~2,100 pw and ~8 years. Both numbers are stated in §6 so
  the reader can choose, but inventing a contingency the domain plans did not ask for would make the
  figure less traceable rather than more honest.
- **It does not re-plan any domain.** Where a domain plan's estimate stands unchallenged it is carried
  through unchanged, including estimates this document suspects are optimistic (MP4 demux at 8 pw
  against 12.5k lines upstream; the T3 filter tail at 1.5 days each).
- **It does not resolve the eighteen open questions in §8.** Those are the user's, and several need
  money or counsel rather than analysis.
- **It does not schedule post-1.0 work** beyond naming it: T3-04…T3-07, T4-\*, T5-01 and SP-C13 total
  ~739 pw, plus ~101 pw of scope reductions if they are later wanted back. That backlog is half the
  size of v1.0 itself and is the right place for the project's second phase to be planned, not this one.
- **It does not decompose the post-1.0 backlog.** §11 split every package over 8 pw inside v1.0 and
  stopped at the v1.0 line, for the reason above. Those six rows are the largest remaining blocks in
  the document and §11.8 records that they must be split before any of them is scheduled.

---

## 11. The decomposition pass: what was split, and why

This section records a single pass over the register whose purpose was to make every row **executable
by one agent under `19-parallel-execution.md`**, and to find out what the critical path actually is
once that is true. It changed no estimate and no scope. It changed how the same 1,782 pw is packaged.

| | Before | After |
|---|---:|---:|
| Register rows | 405 | **614** |
| Dispatchable packages | 400 | **548** |
| Non-dispatchable grouping labels | 0 | 57 |
| Packages over 8 pw (v1.0 scope) | 41 | **0** |
| Largest single package | **60.0 pw** | **8.0 pw** |
| Rows naming two or more explicit crates | 26 | 13 *(all declared crate families, §11.4)* |
| Longest dependency chain | 79.5 wk *(59.5 excluding T3)* | **53.5 wk** |
| Total effort | 1,782.0 pw | **1,782.0 pw** |

### 11.1 The two tentpoles

**T3-01 (H.264, 60 pw) and T3-02 (HEVC, 55 pw)** were the largest unsplit packages in the register and
the reason §3.4 existed. Both are split along **the specifications' own structure**, which is the only
seam that gives independently testable children: each child can be gated against a slice of the JVT or
JCT-VC conformance corpus without the rest of the decoder being finished.

| Layer | H.264 | HEVC |
|---|---|---|
| Bitstream / NAL layer | T3-01a (4.0) | T3-02a (3.0) |
| Parameter sets | T3-01b (3.0) | T3-02b (3.0) |
| Slice header (+ RPS for HEVC) | T3-01c (3.0) | T3-02c (4.0) |
| Entropy — CAVLC | T3-01d (5.0) | *(n/a — HEVC is CABAC-only)* |
| Entropy — CABAC | T3-01e (6.0) | T3-02d (7.0) |
| Block-structure layer | T3-01f macroblock (5.0) | T3-02e CTU quadtree (5.0) |
| Intra prediction | T3-01g (5.0) | T3-02f (4.0) |
| Inter — MV derivation | T3-01h (4.0) | T3-02g merge/AMVP (4.0) |
| Inter — sample interpolation | T3-01i (4.0) | T3-02h (4.0) |
| Transform + dequant | T3-01j (4.0) | T3-02i (4.0) |
| Deblocking | T3-01k (4.0) | T3-02j (3.0) |
| SAO | *(n/a)* | T3-02k (2.0) |
| Reference management + DPB | T3-01l (6.0) | T3-02l (4.0) |
| Threading + profile extensions + integration | T3-01m (4.0) | T3-02m (5.0) |
| Conformance | T3-01n (3.0) | T3-02n (3.0) |
| **Total** | **60.0** ✓ | **55.0** ✓ |

**Why inter prediction is two packages and not one.** In both codecs the candidate-derivation half
(MV prediction, merge/AMVP, direct modes) and the sample half (sub-pel interpolation, weighted
prediction) share nothing but the PU geometry. Splitting them takes four weeks off both chains,
because they then run beside each other instead of behind each other. This is the single most
schedule-productive seam in the pass.

**Why CAVLC and CABAC are separate packages.** They are separate spec clauses (§9.2 and §9.3), they
have disjoint test corpora, and CABAC is the largest single child in either codec. Keeping them
together would have re-created an 11 pw block on the critical path.

T3-03 (AAC, 30 pw) is split the same way — configuration, LC core syntax, reconstruction, SBR,
Parametric Stereo, conformance — with SBR deliberately depending on the configuration layer rather
than on the LC core, because the QMF banks are testable against their own vectors.

### 11.2 Everything else that was split

Fifty-five further parents. The criterion each one tripped: **A** over ~8 pw, **B** spans more than one
crate directory (`19-parallel-execution.md` §2), **C** on or near a long chain and parallelisable,
**D** has internally verifiable phases.

| Parent | pw | Crit. | Children | Seam used |
|---|---:|---|---:|---|
| D-08 | 8.0 | A C | 2 | engine + scalar reference / tier-specific SIMD. **On the critical path when found.** |
| D-21 | 4.0 | B | 2 | two unrelated CBS crates |
| D-22 | 14.0 | A D | 4 | context model / MB loop / MC / resilience + per-family validation |
| C-13 | 6.0 | B | 3 | three independent image crates |
| C-15 | 10.0 | A D | 3 | baseline / progressive+12-bit / MJPEG + encoder |
| C-16 | 10.0 | A D | 4 | header+entropy / intra+transform / inter / loop filter + conformance |
| C-17 | 12.0 | A D | 4 | skeleton / intra+RDO / inter+ME / rate control + gating |
| C-21 | 12.0 | A D | 4 | codebooks / psychoacoustics / residue / bitrate + gating |
| C-26 | 20.0 | A D | 6 | framing / CELT analysis / CELT allocation / SILK NSQ / hybrid+RC / gating |
| C-32 | 9.0 | A C | 3 | loop filter / profiles 1–3 / threading + conformance |
| C-33 | 22.0 | A C D | 6 | skeleton / partition+intra / inter+ME / transform+RDO / rate control / gating |
| C-39 | 12.0 | A C | 3 | MV stack / interpolation + compound / warp + OBMC |
| C-41 | 8.0 | C | 4 | deblock / CDEF / superres / loop restoration — four independent post-filters |
| T2-01 | 12.0 | A D | 3 | MPEG-1/2 decode / MPEG-2 extensions / encode |
| T2-02 | 12.0 | A B | 4 | H.261+H.263 / H.263+ annexes / MPEG-4 P2 decode / encode |
| T2-03 | 14.0 | A D | 5 | Layer I–II / Layer III / free-format+gapless / MP2 enc / MP3 enc |
| T2-04 | 12.0 | A D | 4 | AC-3 bitstream / AC-3 reconstruction / **E-AC-3 (D9-gated)** / conformance |
| T2-07 | 16.0 | A D | 4 | codestream syntax / EBCOT T1 / T2+DWT / encode |
| T2-09 | 14.0 | A B | 4 | ProRes ×2 / DNxHD ×2 |
| T2-10 | 16.0 | A D | 5 | headers / entropy+MB / intra+transform / inter / loop filter + conformance |
| T2-11 | 12.0 | A D | 4 | syntax / wavelet+coefficients / arithmetic+MC / encode |
| T2-13 | 14.0 | A B | 5 | DVD+DVB / PGS / CEA-608 / CEA-708 / Teletext, across three crates |
| T2-14 | 10.0 | A B | 3 | APV decode / APV encode / JPEG XS |
| H-02 | 11.0 | A D | 3 | session+buffers / decode / encode |
| H-06 | 12.0 | A D | 3 | device bring-up / decode / encode |
| H-03, H-04, H-05 | 11.0 ea. | A D | 2 ea. | device+decode / encode *(conditional)* |
| FT-4.1 | 5.0 | B | 3 | three filter crates |
| FT-4.6 | 6.0 | B | 2 | blur / denoise |
| FT-4.8 | 9.0 | A B | 2 | EQ / dynamics |
| FT-4.12 | 34.0 | A B D | 7 | seven thematic filter groups, ~20 filters each |
| FT-4.13 | 14.0 | A B D | 5 | five thematic audio-filter groups |
| FT-5.6 | 10.0 | A D | 3 | kernel harness + reference kernel / colour kernels / spatial kernels |
| FT-5.7 | 10.0 | A D | 3 | elementary features / model+fusion / variants |
| PR-09 | 10.0 | A D | 3 | chunk layer / AMF+commands / tunnelled variants |
| PR-10 | 12.0 | A D | 3 | framing+handshake / congestion control / modes+interop |
| PR-11 | 10.0 | A D | 3 | simple profile / main profile / bonding |
| PR-12 | 8.0 | B | 3 | three protocol crates |
| FM-26, FM-28, FM-29, FM-30, FM-31, FM-32, FM-35 | 4.0–9.0 | B D | 2 ea. | **demux / mux** — the cleanest seam in the whole register: separate crates, separate acceptance checks (packet-stream identity vs. byte identity) |
| FM-50 | 14.0 | A C D | 4 | KLV / structural metadata / essence containers / index+seek |
| FM-51 | 12.0 | A D | 3 | KLV writer / OP1a wrapping+index / d10+opatom+remux matrix |
| FM-55 | 10.0 | A B | 3 | GXF / IMF manifests / IMF essence |
| FM-57 | 8.0 | B C | 3 | Smooth Streaming / HDS / WHIP |
| FM-58, FM-59 | 10.0 ea. | A D | 3 ea. | thematic container groups |
| CL-26 | 2.0 | B | 2 | `vaco-sched` half / `vaco` half — **a genuine cross-crate write, not a family** |
| CL-34 | 6.0 | A B | 2 | `vaco` half / `vaco-probe` half |

### 11.3 What the recomputation found

Recomputing the critical path mechanically from the `Deps` column — rather than by hand — surfaced
three things the previous revision did not have:

1. **A dependency cycle.** `PF-3.2` (batched MC dispatch) listed `D-08` as a dependency while `D-08`
   listed `PF-3.2`. §5.4 band D item 6 is explicit that PF-3.2 must settle *before* F-02's DSP traits
   freeze, so the correct edge is `PF-3.2 → D-08`, not both. **Fixed:** PF-3.2 now depends on
   `F-01, PF-0.1`.
2. **The real critical path was never AV1 or MXF.** It was **VP9 decode → encode at 59.5 weeks**
   (`F-03 → D-08 → C-31 → C-32 → C-33`), four weeks longer than the 55 the headline claimed and
   invisible because it was three large blocks in a row rather than one very large one. Splitting D-08,
   C-32 and C-33 — and correcting two dependencies that were serial only by accident — brought it to
   49.5.
3. **Two published chains did not survive the check** (§3.3.1): MXF was never 55.5 weeks, and chain γ's
   stated total disagreed with its own listed items.

The net: **79.5 → 53.5 weeks**, with the binding chain now HEVC and three others within two weeks of it.

### 11.4 Multi-crate packages and single-writer ownership

`19-parallel-execution.md` §2 says an agent owns **one crate directory and nothing else**. Twenty-six
rows named two or more explicit crates, and a further handful named a glob (`vaco-protocol-*`,
`vaco-codec-dsp-*`). Read literally that is a correctness problem, not a sizing one. Two different situations were hiding under one symptom:

- **Genuine multi-writer risk** — the crates are substantial and independently dispatchable, so two
  packages could end up writing one crate, or one package writing two crates another package also
  wants. These were **split per crate**: D-21, C-13, T2-02, T2-09, T2-13, T2-14, FT-4.1, FT-4.6,
  FT-4.8, PR-12, the seven demux/mux pairs, FM-55, FM-57, CL-26, CL-34.
- **A crate family** — several tiny sibling crates that no other package touches, where splitting
  would produce 0.3 pw packages and buy nothing. **These are kept, and named as an explicit,
  documented exception:** one agent owns the whole family, which preserves the rule's actual
  invariant (one writer per file) while avoiding fragmentation. **Thirteen rows name two or more
  crates and are declared families:** `FD-05` (`vaco-opts` + its derive — a proc-macro crate cannot be
  co-designed by a second agent), `FD-08`, `F-04`, `QA-05`, `PF-4.6`, `C-03`, `B-01`, `B-03`, `B-06`,
  `SH-08`, `FT-4.1c`, `FT-4.3`, `FT-4.11`. The glob rows (`PR-01`, `PR-02`, `PR-04`, `PR-07`,
  `PF-3.1`, `PF-3.10`, `PF-4.5`, `FT-4.2`, `FT-4.12a`–`g`, `FT-4.13a`–`e`) are families of the same
  kind and are governed by the same rule; the orchestrator expands each glob into an explicit crate
  list in `ASSIGNMENTS.md` at dispatch, because a glob is not an ownership record.

**The orchestrator records a family as one ownership row in `ASSIGNMENTS.md`, listing every crate in
it.** A family is only safe while no other package writes into it, and that is the orchestrator's
check, not the agent's.

### 11.5 The reverse problem: four merges

Fragmentation costs orchestration overhead and context-switching, and a 0.5 pw package is a wave
boundary's worth of ceremony for two days of work. Four rows were merged into a sibling:

| Merged | Into | Why |
|---|---|---|
| FD-09 `vaco-chlayout` (1.5) | **FD-08** (now 2.0) | Two tiny audio-descriptor crates with the same dependency (FD-01) and the same two consumers (FD-11, SP-B1). |
| SH-10 `vaco-format-replaygain` (0.5) | **SH-08** (now 1.5) | ReplayGain is carried inside APE tags and Vorbis comments; it is a field set, not a subsystem. |
| C-47 `vnull`/`anull` (0.5) | **C-03** (now 4.5) | Two trivial pass-through codecs alongside the other pass-through codec crate. |
| PF-0.7 `VACO_TIER` + size budget (0.5) | **PF-4.6** (now 2.5) | Both are CI/startup/binary-size concerns on the same crate pair, and PF-4.6 already owns the binary-size question. |

**Deliberately not merged:** `P0-05` (0.5) is the orchestrator's own process work, on a different
owner and a different dependency; `PF-0.0` (0.5) is a blocking spike with a hard escalation gate
(">3× ⇒ stop and escalate") and merging it would bury that gate inside a larger package.

No child created by this pass is below 1.0 pw.

### 11.6 Arithmetic audit

**Every set of children sums exactly to its parent.** No estimate was revised — where this document
suspected a figure was optimistic it said so in §10 and left the number alone, and that discipline is
preserved here. The check is mechanical and reproducible: for each *(group)* row, sum the pw of the
lettered rows beneath it and compare. All 57 groups pass, and all sixteen section subtotals in §1 are
unchanged:

| Section | Subtotal | Unchanged? |
|---|---:|---|
| Wave 0 / Foundations / Performance / Correctness / CLI | 5.0 / 24.0 / 66.0 / 33.5 / 75.0 | ✓ |
| Framework+harness / DSP+CBS / parsers / T1 codecs | 23.0 / 98.0 / 19.0 / 288.5 | ✓ |
| BSF+T2+hardware (uncond.) / T3 in v1.0 | 219.0 / 145.0 | ✓ |
| Filters / Signal processing | 224.0 / 108.0 | ✓ |
| I/O+protocols+framework / Containers | 142.5 / 311.5 | ✓ |
| **Register total** | **1,782.0** | ✓ |
| **v1.0 planning case after §1.16's reductions** | **~1,680.0** | ✓ |

### 11.7 The rule that makes the splits executable

Splitting a 60 pw crate into fourteen children creates a hazard the register did not have before:
**fourteen packages that all write `crates/codec/vaco-codec-h264/**`.** Plan 19 §2's answer — "if a
crate needs two people, it needs splitting into two crates first" — is the right default and the wrong
answer here, because H.264's layers are not separable crates; they share the decoder context.

Note that the register **already relied on this** before this pass: §5.6 dispatches "the AV1 chain
C-34…C-44 (five agents after one clears C-34…C-37)" — five agents in `vaco-codec-av1`. The pass makes
the implicit rule explicit rather than inventing it.

**Module-level single-writer ownership, permitted only under all four conditions:**

1. **The first child owns the crate's shared files** — `lib.rs`, `Cargo.toml`, `vaco-component.toml`,
   the context struct, the module declarations — and **freezes them** before any sibling is dispatched.
   In every split above, the `a` child is that child, and its acceptance clause says so.
2. **Every concurrent sibling is assigned a disjoint module path**, recorded in `ASSIGNMENTS.md` as
   `crate::module` rather than as a crate. Two siblings never write one file.
3. **A sibling that needs a change to a shared file stops and reports**, exactly as `19` §8's
   escalation rule requires for a cross-crate change. It does not reach into `lib.rs`.
4. **The integration child** (`m`/`l`/`c`, depending on the split) is dispatched **alone**, after its
   siblings land, and is the only child permitted to touch the shared files again.

This is the same protocol as §2's crate ownership with the unit changed from directory to module, and
it is safe for the same reason: ownership is spatial and assigned, not locked.

### 11.8 What was deliberately left whole

**Eighteen packages sit at exactly 8.0 pw** and were not split: C-20, C-27, C-35, C-38, C-40, T2-05,
T2-06, T2-12, H-01, FT-4.4, FM-01, FM-24, FM-40, FM-41, FM-43, FM-44, FM-45, FM-46. Each is a single
crate and a single coherent specification section, and 8 pw is the stated ceiling rather than a
violation of it. **Three of them are on chains within two weeks of the top** — C-35 (AV1 frame header)
on chain α, FM-40 and FM-41 (RTSP/SDP and the RTP depacketisers) on chain ζ — and they are therefore
the next candidates if the critical path ever needs another two weeks. It does not: §3.5 is right that
the remaining lever is throughput.

**The post-1.0 backlog was not decomposed.** T3-04 (25), T3-05 (12), T3-06 (40), T3-07 (110), T4-\*
(~250) and T5-01 (~300) are all far over the ceiling, and T3-07 and T4-\* are not packages at all —
they are placeholders. Decomposing them now would be planning work against scope the project has not
committed to, and §10 is explicit that scheduling the post-1.0 backlog is the second phase's job, not
this document's. **They must be split before any of them is scheduled**, on the same criteria used
here.

---

## Execution note — wave 2 ordering adjusted (2026-08-22)

Plan 19's wave 2 lists the substrate crates without an internal order. Executed
order puts **`vaco-conformance` first**, ahead of everything else in the wave.

Reason: wave 1 finished with three crates carrying unvalidated data —
`vaco-pixfmt`'s 268-format table, and `vaco-core`'s colour, frame-size and
frame-rate tables. All are internally consistent and physically derived, and none
has been diffed against the reference binary. Plan 11 calls that diff the primary
acceptance criterion. Every further table we build inherits the same gap, so the
harness is worth more now than any additional feature.

Dispatched concurrently (5 agents, at the plan 19 §7 ceiling):

| Agent | Crates | Issues | Rationale |
|---|---|---|---|
| conformance | `vaco-conformance` | #172, #173 | Unblocks validation for three finished crates |
| io | `vaco-io`, `-protocol-core`, `-protocol-file` | #199, #200, #535 | `vaco-format-core` depends on it |
| codec-core | `vaco-codec-core` | #170, #251 | Sits below format-core per D14.1 |
| tx | `vaco-tx` | #243–#246 | Critical path; every audio codec blocks on it |
| textformat | `vaco-textformat` | #188, #189 | *Is* the v0.1 acceptance criterion |

`vaco-format-core` is deliberately held back one dispatch rather than run
concurrently with `vaco-io`. Its probing path depends on `peek` working over a
non-seekable source, which is behaviour rather than signature — and wave 1 showed
what it costs to build against a dependency whose bodies do not yet exist
(`vaco-opts` reimplemented 897 lines of `vaco-core` and threw them away).
