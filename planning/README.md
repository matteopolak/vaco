# Vaco — Planning

A clean-room Rust reimplementation of `ffmpeg`, `ffprobe` and `ffplay`.

This directory is the complete plan: what FFmpeg does, what we will build, how, in what order, and
under which constraints. ~25,000 lines across research, decisions and plans.

---

## Read in this order

| # | Document | What it is |
|---|---|---|
| **1** | [`00-decisions.md`](00-decisions.md) | **Start here. Binding.** D1–D14: the locked constraints every plan conforms to. If a plan and a decision disagree, the decision wins. |
| **2** | [`10-architecture.md`](10-architecture.md) | The keystone. Crate layering, naming, feature model, threading, performance architecture. ~120–160 crates in 8 layers. |
| **3** | [`19-parallel-execution.md`](19-parallel-execution.md) | **How the work actually gets done.** One shared tree, one branch, no worktrees. Ownership, generated shared files, sccache, git protocol. Read before dispatching anyone. |
| **4** | [`20-roadmap.md`](20-roadmap.md) | The integrated schedule. 405 work packages, deduplicated effort, critical path, milestones, risk register, first ten packages. |

## Domain plans

| Document | Scope |
|---|---|
| [`11-foundations.md`](11-foundations.md) | Layers 0–1: core, opts, simd, bitstream, expr, pixfmt, color, frame, packet, pool. Plus workspace, toolchain, lints. |
| [`12-performance.md`](12-performance.md) | SIMD strategy, kernel authoring standard, autovectorization discipline, benchmarks, `vaco-checkasm`, PGO/LTO/BOLT. |
| [`13-correctness.md`](13-correctness.md) | The differential harness, fuzzing, conformance suites, CI, provenance, release engineering. |
| [`14-cli.md`](14-cli.md) | `vaco` / `vaco-probe` / `vaco-play`: option parsing, stream specifiers, output writers, scheduler, player. |
| [`15-codecs.md`](15-codecs.md) | Codec framework, shared DSP crates, tiering, per-codec plans, hardware acceleration. |
| [`16-filters.md`](16-filters.md) | Filter framework, filtergraph DSL, format negotiation, 41 filter crates, GPU filters. |
| [`17-scale-resample-tx.md`](17-scale-resample-tx.md) | `vaco-scale` (ops graph), `vaco-resample`, `vaco-tx`. |
| [`18-formats.md`](18-formats.md) | Containers and protocols: format core, I/O, MP4/Matroska/MPEG-TS, tiering, v0.1 delivery. |

## Research

Feature inventories of FFmpeg 8.0.git, produced under clean-room rules — capability catalogues, never
implementation. See [`research/`](research/):

`01` libavutil/swresample/swscale · `02` libavcodec · `03` libavformat · `04` libavfilter ·
`05` fftools CLI contract · `06` devices/build/test · `07` legal, patents and licensing ·
`08` performance and SIMD · `09` dependency licence register

---

## The project in eight facts

1. **Rust-native, no C ABI.** Idiomatic crates plus three binaries. CLI-compatible with ffmpeg; not ABI-compatible.
2. **`#![forbid(unsafe_code)]` everywhere except `vaco-hw-*`.** Runtime SIMD dispatch is safe via `fearless_simd`'s capability tokens.
3. **GPL-3.0-or-later**, with dependencies gated on pure-Rust (zero FFI), permissive licence, and active maintenance.
4. **Byte-identical output against reference ffmpeg** where determinism exists; quality-band comparison where it does not (lossy encode never matches).
5. **Royalty-free default build.** Patent-encumbered encoders exist in-tree behind named feature flags and never ship.
6. **Hardware acceleration ships by default** — it is how users get H.264 and HEVC from a binary containing no software implementation of either.
7. **~50% of FFmpeg's inventory needs a spec-extraction pass first.** 300 of 605 decoders and 192 of 368 demuxers have no public specification. They are *legally* implementable (D15) — the constraint is cost and demand, not copyright. v1.0's scope is bounded by effort, which the user controls.
8. **~1,680 person-weeks to v1.0**, across **548 dispatchable work packages**, none larger than 8 pw. The critical path is only ~53.5 weeks — 16% of the calendar — so this project is throughput-bound, not dependency-bound, which makes concurrency a purchasing decision rather than an engineering one.

## Open questions for the user

Ranked by how much work each blocks — see `20-roadmap.md` §8 for the full set.

1. **The counsel package.** Blocks the legitimacy of everything. Longest lead time. Start now.
2. **Hardware budget / agent concurrency.** Sets the schedule's slope directly.
3. **What v1.0 means**, given parity is unreachable.
4. **TLS crypto provider** — gates 53.5 pw.
5. **Whether H.264/HEVC belong in v1.0 at all** — dropping them buys 115 pw of throughput and costs only one week of calendar.
