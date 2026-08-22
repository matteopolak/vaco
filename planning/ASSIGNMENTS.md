# Assignments

One row per crate. **The orchestrator is the only writer of this file.**

Ownership is spatial (plan 19 §2): an agent owns one crate directory and writes
nowhere else. Two agents are never assigned the same crate concurrently — if a
crate needs two people it needs splitting into two crates first.

Status: `free` · `assigned` · `in-review` · `done`

**`frozen` is not `done`.** Wave 0 froze every public signature with a `todo!()`
body. A crate whose interface is frozen but whose bodies are unimplemented is
`free`, not `done` — recording it as done cost real work: the `vaco-opts` agent
found `vaco-core`'s `Rational` methods still unimplemented and had to reimplement
`Dict`, `escape` and `parse` locally to make progress.

| Crate | Layer | Issue | Owner | Status | Started | Finished | Note |
|---|---|---|---|---|---|---|---|
| vaco-core | 0 | — | agent:core | assigned | 2026-08-21 | | frozen in P0-03; bodies outstanding |
| vaco-limits | 0 | — | agent:bitstream | assigned | 2026-08-21 | |
| vaco-simd | 0 | #90 | agent:simd | done | 2026-08-21 | 2026-08-21 | x86 re-run outstanding |
| vaco-opts | 0 | — | agent:opts | done | 2026-08-21 | 2026-08-21 | |
| vaco-opts-derive | 0 | — | agent:opts | done | 2026-08-21 | 2026-08-21 | |
| vaco-expr | 0 | — | — | free | | |
| vaco-bitstream | 0 | — | agent:bitstream | assigned | 2026-08-21 | |
| vaco-pixfmt | 1 | — | agent:pixfmt | done | 2026-08-21 | 2026-08-21 |
| vaco-sampfmt | 1 | — | — | free | | |
| vaco-chlayout | 1 | — | — | free | | |
| vaco-color | 1 | — | — | free | | |
| vaco-pool | 1 | — | — | free | | |
| vaco-frame | 1 | — | — | free | | |
| vaco-packet | 1 | — | — | free | | |
| vaco-io | 2 | — | — | free | | |
| vaco-protocol-core | 2 | — | — | free | | |
| vaco-protocol-file | 2 | — | — | free | | |
| vaco-protocol-http | 2 | — | — | free | | |
| vaco-tx | 3 | — | — | free | | |
| vaco-scale | 3 | — | — | free | | |
| vaco-resample | 3 | — | — | free | | |
| vaco-codec-core | 3 | — | — | free | | | frozen only |
| vaco-codec-golomb | 3 | — | — | free | | |
| vaco-codec-cabac | 3 | — | — | free | | |
| vaco-codec-cbs | 3 | — | — | free | | |
| vaco-codec-dsp-idct | 3 | — | — | free | | |
| vaco-format-core | 4 | — | — | free | | | frozen only |
| vaco-format-riff | 4 | — | — | free | | |
| vaco-format-isom | 4 | — | — | free | | |
| vaco-format-mpegts-tables | 4 | — | — | free | | |
| vaco-format-id3 | 4 | — | — | free | | |
| vaco-format-nalu | 4 | — | — | free | | |
| vaco-parse-h264 | 4 | — | — | free | | |
| vaco-parse-hevc | 4 | — | — | free | | |
| vaco-parse-av1 | 4 | — | — | free | | |
| vaco-parse-aac | 4 | — | — | free | | |
| vaco-parse-opus | 4 | — | — | free | | |
| vaco-demux-mp4 | 4 | — | — | free | | |
| vaco-demux-matroska | 4 | — | — | free | | |
| vaco-demux-mpegts | 4 | — | — | free | | |
| vaco-filter-core | 5 | — | — | free | | | frozen only |
| vaco-filter-framesync | 5 | — | — | free | | |
| vaco-filter-graph | 5 | — | — | free | | |
| vaco-registry | 6 | — | — | free | | |
| vaco-textformat | 7 | — | — | free | | |
| vaco-cli-core | 7 | — | — | free | | |
| vaco-sched | 7 | — | — | free | | |
| vaco-probe | 7 | — | — | free | | |
| vaco-cli | 7 | — | — | free | | |
| vaco-conformance | 10 | — | — | free | | |
| vaco-checkasm | 10 | — | — | free | | |
